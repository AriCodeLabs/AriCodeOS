use aricode_arifs::{AriFs,BlockDevice,FsError,SECTOR};
struct Disk(Vec<u8>);
impl BlockDevice for Disk {
 fn read_sector(&mut self,lba:u64,b:&mut[u8;SECTOR])->Result<(),FsError>{let o=(lba as usize+34)*SECTOR;b.copy_from_slice(&self.0[o..o+SECTOR]);Ok(())}
 fn write_sector(&mut self,lba:u64,b:&[u8;SECTOR])->Result<(),FsError>{let o=(lba as usize+34)*SECTOR;self.0[o..o+SECTOR].copy_from_slice(b);Ok(())}
 fn flush(&mut self)->Result<(),FsError>{Ok(())}
}
fn section(b:&mut Vec<u8>,id:u8,data:&[u8]){b.extend([id,data.len() as u8]);b.extend(data);}
fn main(){
 let path=std::env::args().nth(1).unwrap();let mut fs=AriFs::open(Disk(std::fs::read(&path).unwrap())).unwrap();
 let elf=std::fs::read("review_app.elf").unwrap();
 let mut seed=[0;32];aricode_crypto::unhex(std::fs::read_to_string("/var/tmp/aricode-review-6ee19fb-uhBqbF/keys/dev/ed25519.seed").unwrap().trim(),&mut seed).unwrap();
 for name in ["reviewa","reviewb"]{
  let m=format!("name = {name}\ncaps = fs\nmemory = 64K\n");let pkg=aricode_pkg::build(name,1,&elf,m.as_bytes(),&seed,false).unwrap();
  let i=fs.create(&format!("apps/{name}.aripkg")).unwrap();fs.replace(i,&pkg).unwrap();
 }
 let mut wasm=b"\0asm\x01\0\0\0".to_vec();
 section(&mut wasm,1,&[2,0x60,0,1,0x7f,0x60,1,0x7e,1,0x7f]);
 let mut imp=vec![1,7];imp.extend(b"aricode");imp.push(10);imp.extend(b"file_write");imp.extend([0,0]);section(&mut wasm,2,&imp);
 section(&mut wasm,3,&[1,1]);section(&mut wasm,7,&[1,4,b'm',b'a',b'i',b'n',0,1]);section(&mut wasm,10,&[1,4,0,0x10,0,0x0b]);
 assert!(aricode_wasm::Module::parse(&wasm).is_ok());
 std::fs::write("wrong-import.wasm",&wasm).unwrap();
 let i=fs.find("wasm_runner/demo.wasm").unwrap();fs.replace(i,&wasm).unwrap();
 fs.flush().unwrap();std::fs::write(path,fs.into_device().0).unwrap();
}
