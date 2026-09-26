//! UDP (RFC 768) con checksum obligatorio en emisión.

use crate::ipv4::{pseudo_header_sum, PROTO_UDP};
use crate::{be16, checksum, Ipv4Addr};

pub const HEADER_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub src_port: u16,
    pub dst_port: u16,
}

impl Header {
    /// Analiza y valida longitud y checksum (si no es 0). Devuelve la carga útil.
    pub fn parse(src: Ipv4Addr, dst: Ipv4Addr, b: &[u8]) -> Option<(Header, &[u8])> {
        if b.len() < HEADER_LEN {
            return None;
        }
        let len = be16(&b[4..6]) as usize;
        if len < HEADER_LEN || len > b.len() {
            return None;
        }
        let sum = be16(&b[6..8]);
        if sum != 0
            && checksum(
                pseudo_header_sum(src, dst, PROTO_UDP, len as u16),
                &b[..len],
            ) != 0
        {
            return None;
        }
        Some((
            Header {
                src_port: be16(&b[0..2]),
                dst_port: be16(&b[2..4]),
            },
            &b[HEADER_LEN..len],
        ))
    }

    /// Escribe cabecera y carga útil con checksum sobre la pseudocabecera.
    pub fn write(
        &self,
        src: Ipv4Addr,
        dst: Ipv4Addr,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let len = HEADER_LEN + payload.len();
        if out.len() < len || len > u16::MAX as usize {
            return None;
        }
        out[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        out[4..6].copy_from_slice(&(len as u16).to_be_bytes());
        out[6..8].copy_from_slice(&[0, 0]);
        out[HEADER_LEN..len].copy_from_slice(payload);
        let mut sum = checksum(
            pseudo_header_sum(src, dst, PROTO_UDP, len as u16),
            &out[..len],
        );
        if sum == 0 {
            sum = 0xFFFF;
        }
        out[6..8].copy_from_slice(&sum.to_be_bytes());
        Some(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_round_trip_with_checksum() {
        let src = Ipv4Addr::new(10, 0, 2, 15);
        let dst = Ipv4Addr::new(10, 0, 2, 2);
        let h = Header {
            src_port: 7,
            dst_port: 17007,
        };
        let mut buf = [0u8; 64];
        let n = h.write(src, dst, b"hola", &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_ne!(be16(&buf[6..8]), 0);
        let (parsed, payload) = Header::parse(src, dst, &buf[..n]).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload, b"hola");
        // Otra direccion de destino invalida el checksum.
        assert!(Header::parse(src, Ipv4Addr::new(10, 0, 2, 3), &buf[..n]).is_none());
        // Checksum 0 = ausente: se acepta.
        buf[6..8].copy_from_slice(&[0, 0]);
        assert!(Header::parse(src, dst, &buf[..n]).is_some());
    }
}
