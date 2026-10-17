// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
#![no_std]
#![no_main]
use aricode_rt::sys::{self,Handle,kind,permissions,rights,rpc};
use aricode_rt::sys::fs::{FsClient,FsError};
use aricode_proto::fs::{Request,Reply,Op,Status};
fn main(arg:u64)->u64 {
 let(mut ep,mut hw)=(None,None);
 permissions::granted(|h,_|{if permissions::is(h,kind::ENDPOINT){ep=Some(h)}else if permissions::is(h,kind::HARDWARE){hw=Some(h)}});
 let(Some(ep),Some(hw))=(ep,hw)else{return 1};let Ok(mut fs)=FsClient::new(ep,hw)else{return 2};
 let Ok(fd)=fs.open("probe").or_else(|_|fs.create("probe"))else{return 3};
 if arg==1 {let r=fs.replace(fd,b"SYNTHETIC_PRIVATE_DATA");let _=fs.close(fd);sys::print(b"review: A wrote private fixture\n");return if r.is_ok(){0}else{4};}
 if arg==4 {let r=fs.replace(fd,b"new content");let _=fs.close(fd);sys::print(if matches!(r,Err(FsError::Status(Status::Busy))){b"review: BUSY after other app exited\n"}else{b"review: replacement available\n"});return 0;}
 let Ok(dma)=sys::dma_alloc(hw,4096)else{return 5};let Ok(v)=sys::mmio_map(dma)else{return 6};
 unsafe{*(v as *mut u8)=b'X';}
 let Ok(transfer)=sys::handle_duplicate(dma,rights::READ|rights::WRITE|rights::TRANSFER)else{return 7};
 let mut req=Request::new(Op::ReplaceChunk);req.fd=fd;req.len=1;let mut out=[0;Reply::LEN];
 let Ok(n)=rpc::call(ep,&req.encode(),&[transfer],5000,5000,&mut out)else{return 8};
 if Reply::decode(&out[..n]).map(|r|r.status)!=Some(Status::Ok){return 9;}
 if arg==3 {sys::print(b"review: exiting after chunk without commit/close\n");return 0;}
 req=Request::new(Op::ReplaceCommit);req.fd=fd;req.len=22;
 let Ok(n)=rpc::call(ep,&req.encode(),&[],5000,5000,&mut out)else{return 10};
 if Reply::decode(&out[..n]).map(|r|r.status)!=Some(Status::Ok){sys::print(b"review: partial commit rejected\n");return 0;}
 let mut bytes=[0;32];let Ok(n)=fs.read(fd,0,&mut bytes)else{return 11};
 sys::print(b"review: B read from its own file: ");sys::print(&bytes[..n]);sys::print(b"\n");let _=fs.close(fd);0
}
aricode_rt::entry!(main);
