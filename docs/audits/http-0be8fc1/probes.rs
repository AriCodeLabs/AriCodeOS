pub mod net {
    use std::collections::VecDeque;
    use aricode_proto::net::Status;
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NetError { Status(Status) }
    pub struct NetClient {
        pub incoming: VecDeque<Vec<u8>>,
        pub end: Result<usize, NetError>,
        pub sent: Vec<u8>,
    }
    impl NetClient {
        pub fn new(parts: Vec<Vec<u8>>) -> Self {
            Self { incoming: parts.into(), end: Ok(0), sent: vec![] }
        }
        pub fn resolve(&self, _: &str, _: Option<([u8; 4], u16)>, _: u32) -> Result<[u8;4], NetError> { Ok([127,0,0,1]) }
        pub fn tcp_connect(&self, _: [u8;4], _: u16, _: u32) -> Result<u16, NetError> { Ok(1) }
        pub fn tcp_close(&self, _: u16) -> Result<(), NetError> { Ok(()) }
        pub fn tcp_send_all(&mut self, _: u16, data: &[u8]) -> Result<(), NetError> { self.sent.extend_from_slice(data); Ok(()) }
        pub fn tcp_recv(&mut self, _: u16, _: u32, out: &mut [u8]) -> Result<usize, NetError> {
            if out.is_empty() { return Ok(0); }
            match self.incoming.pop_front() {
                Some(data) => {
                    let n = out.len().min(data.len()).min(aricode_proto::net::MAX_DATAGRAM as usize);
                    out[..n].copy_from_slice(&data[..n]);
                    if n < data.len() { self.incoming.push_front(data[n..].to_vec()); }
                    Ok(n)
                },
                None => self.end,
            }
        }
    }
}
// The SDK's `http.rs`: after the fix it only adapts `NetClient` to the
// `Transport` of `aricode-http`; the HTTP logic lives in that crate and is tested there.
#[path = "__ARICODEOS_ROOT__/sdk/rust/src/http.rs"]
pub mod http;

#[cfg(test)]
mod tests {
    use super::{http, net::{NetClient, NetError}};
    use aricode_proto::net::Status;
    fn get(parts: Vec<Vec<u8>>, capacity: usize) -> Result<http::Fetched, http::HttpError> {
        http::get(&mut NetClient::new(parts), "http://127.0.0.1/", &mut vec![0; capacity], 100)
    }
    #[test]
    fn nominal_length_response() {
        assert_eq!(get(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec()], 5), Ok(http::Fetched{status:200,body_len:5}));
    }
    #[test]
    fn chunk_4096_fits_caller_buffer() {
        let mut raw=b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1000\r\n".to_vec();
        raw.extend(vec![b'x';4096]); raw.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(get(vec![raw],4096), Ok(http::Fetched{status:200,body_len:4096}));
    }
    #[test]
    fn close_delimited_body_exactly_fits() {
        assert_eq!(get(vec![b"HTTP/1.0 200 OK\r\n\r\nhello".to_vec()],5), Ok(http::Fetched{status:200,body_len:5}));
    }
    #[test]
    fn reset_is_not_successful_completion() {
        let mut net=NetClient::new(vec![b"HTTP/1.0 200 OK\r\n\r\npartial".to_vec()]);
        net.end=Err(NetError::Status(Status::Reset));
        let result=http::get(&mut net,"http://127.0.0.1/", &mut [0;128],100);
        assert!(result.is_err(), "returned {result:?} after reset");
    }
    #[test]
    fn informational_response_is_not_final() {
        let raw=b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec();
        assert_eq!(get(vec![raw],128), Ok(http::Fetched{status:200,body_len:5}));
    }
    #[test]
    fn host_header_preserves_long_hostname() {
        let host=format!("{}.{}.test", "a".repeat(40), "b".repeat(40));
        let mut net=NetClient::new(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()]);
        http::get(&mut net,&format!("http://{host}/"),&mut [0;16],100).unwrap();
        let sent=String::from_utf8(net.sent).unwrap();
        assert!(sent.contains(&format!("Host: {host}\r\n")), "request: {sent:?}");
    }
    #[test]
    fn dns_request_preserves_hostname() {
        // After the fix the name no longer travels in the IPC message (which
        // truncated it to 40 bytes) but in the client buffer; the DNS query
        // that netd builds keeps it whole.
        let host = format!("{}.example.test", "a".repeat(30));
        let mut q = [0u8; 512];
        let n = aricode_net::dns::write_query(1, &host, &mut q).unwrap();
        let mut name = [0u8; aricode_net::dns::MAX_NAME];
        assert_eq!(aricode_net::dns::query_name(&q[..n], &mut name), Some(host.as_str()));
        assert!(aricode_proto::net::Request::LEN == 64, "the message no longer carries a name field");
    }
    #[test]
    fn chunk_size_arithmetic_does_not_panic() {
        let input=format!("{:x}\r\n",usize::MAX - 10);
        let caught=std::panic::catch_unwind(|| {
            let mut dec = aricode_http::ChunkDecoder::new();
            dec.feed(input.as_bytes(), &mut [0;8])
        });
        assert!(caught.is_ok(), "chunk length caused a panic");
        assert!(caught.unwrap().is_err(), "an absurd chunk length is rejected");
    }
    #[test]
    fn transfer_encoding_not_overridden_by_content_length_order() {
        use aricode_http::{parse_response,BodyLength};
        let a=parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 50\r\n\r\n").unwrap();
        assert!(a.is_err() || a.unwrap().body == BodyLength::Chunked, "must reject ambiguity or use transfer encoding");
    }
}
