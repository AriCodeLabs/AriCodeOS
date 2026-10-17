// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
#[cfg(test)]
mod tests {
use aricode_arifs::{AriFs,BlockDevice,FsError,SECTOR,TABLE_LBA};
use aricode_wasm::{Module,Instance,Host,Value,Error};
#[derive(Clone)]
struct Disk { stable: Vec<[u8;SECTOR]>, pending: Vec<(u64,[u8;SECTOR])> }
impl Disk {
 fn new() -> Self {Self{stable:vec![[0;SECTOR];1024],pending:vec![]}}
 fn crash_tables_only(mut self) -> Self {
  for (lba,data) in self.pending.drain(..) {if (TABLE_LBA..TABLE_LBA+4).contains(&lba){self.stable[lba as usize]=data;}}
  self
 }
}
impl BlockDevice for Disk {
 fn read_sector(&mut self,lba:u64,buf:&mut[u8;SECTOR])->Result<(),FsError>{
  *buf=self.pending.iter().rev().find(|(p,_)|*p==lba).map(|(_,b)|b).unwrap_or(&self.stable[lba as usize]).to_owned();Ok(())
 }
 fn write_sector(&mut self,lba:u64,buf:&[u8;SECTOR])->Result<(),FsError>{self.pending.push((lba,*buf));Ok(())}
 fn flush(&mut self)->Result<(),FsError>{for(lba,b)in self.pending.drain(..){self.stable[lba as usize]=b;}Ok(())}
}
#[test]
fn journal_must_not_reuse_another_files_payload(){
 let mut fs=AriFs::format(Disk::new(),1024).unwrap();
 let a=fs.create("a/private").unwrap();let b=fs.create("b/output").unwrap();
 fs.replace(a,b"SYNTHETIC_PRIVATE_DATA").unwrap();
 fs.replace_chunk(b,0,b"X").unwrap();
 let result=fs.replace_commit(b,22);
 let mut out=[0;22];fs.read(b,0,&mut out).unwrap();
 println!("partial_commit={result:?}, second_file={:?}",String::from_utf8_lossy(&out));
 assert!(result.is_err(),"must reject a commit larger than initialized data");
}
#[test]
fn allocation_must_survive_reordered_sector_persistence(){
 let mut fs=AriFs::format(Disk::new(),1024).unwrap();
 fs.create("first").unwrap();
 let mut reopened=AriFs::open(fs.into_device().crash_tables_only()).unwrap();
 let first=reopened.find("first").unwrap();let second=reopened.create("second").unwrap();
 println!("first_lba={}, second_lba={}",reopened.entry(first).unwrap().start_lba,reopened.entry(second).unwrap().start_lba);
 assert_ne!(reopened.entry(first).unwrap().start_lba,reopened.entry(second).unwrap().start_lba);
}
#[test]
fn grown_file_must_not_publish_unpersisted_data(){
 let mut fs=AriFs::format(Disk::new(),1024).unwrap();let i=fs.create("file").unwrap();fs.flush().unwrap();
 fs.write(i,0,b"NEW").unwrap();let mut reopened=AriFs::open(fs.into_device().crash_tables_only()).unwrap();
 let mut out=[0;3];let n=reopened.read(i,0,&mut out).unwrap();
 println!("length={n}, content={out:?}");assert!(n==0||out==*b"NEW");
}
fn section(out:&mut Vec<u8>,id:u8,b:&[u8]){assert!(b.len()<128);out.extend([id,b.len() as u8]);out.extend(b);}
fn header()->Vec<u8>{b"\0asm\x01\0\0\0".to_vec()}
#[test]
fn reject_wrong_global_initializer(){
 let mut b=header();section(&mut b,6,&[1,0x7f,0,0x42,1,0x0b]);
 std::fs::write("wrong-global.wasm",&b).unwrap();assert!(Module::parse(&b).is_err());
}
#[test]
fn reject_missing_memory_export(){
 let mut b=header();section(&mut b,7,&[1,1,b'm',2,99]);
 std::fs::write("missing-memory.wasm",&b).unwrap();assert!(Module::parse(&b).is_err());
}
#[test]
fn reject_inverted_memory_limits(){
 let mut b=header();section(&mut b,5,&[1,1,2,1]);
 std::fs::write("inverted-limits.wasm",&b).unwrap();assert!(Module::parse(&b).is_err());
}
struct NoHost;impl Host for NoHost{fn call(&mut self,_:usize,_:&[Value],_:&mut[u8])->Result<Value,Error>{Err(Error::MissingImport)}}
#[test]
fn valid_loop_parameters_preserved_on_branch(){
 let mut b=header();section(&mut b,1,&[2,0x60,0,1,0x7f,0x60,1,0x7f,1,0x7f]);
 section(&mut b,3,&[1,0]);section(&mut b,7,&[1,4,b'm',b'a',b'i',b'n',0,0]);
 let body=[1,1,0x7f,0x41,2,0x03,1,0x22,0,0x41,1,0x6b,0x22,0,0x20,0,0x0d,0,0x0b,0x0b];
 let mut code=vec![1,body.len() as u8];code.extend(body);section(&mut b,10,&code);
 std::fs::write("loop-params.wasm",&b).unwrap();
 let m=Module::parse(&b).unwrap();let mut mem=[];let mut i=Instance::new(&m,&mut mem).unwrap();
 let result=i.call("main",&[],&mut mem,&mut NoHost);println!("loop result={result:?}");assert_eq!(result,Ok(0));
}
}
