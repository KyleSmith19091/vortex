/// Minimal HPACK decoder for extracting `:path` from HTTP/2 HEADERS frames.
///
/// Only implements enough of RFC 7541 to locate the `:path` pseudo-header on a
/// fresh connection (empty dynamic table). Does not maintain a dynamic table.
use std::sync::LazyLock;

/// HPACK static-table index for `:path`.
const PATH_INDEX: usize = 4;

// ---------------------------------------------------------------------------
// Huffman table from RFC 7541 Appendix B — (code, bit_length) indexed by sym.
// ---------------------------------------------------------------------------
#[rustfmt::skip]
const HUFFMAN_TABLE: [(u32, u8); 257] = [
    (0x1ff8, 13),     (0x7fffd8, 23),   (0xfffffe2, 28),  (0xfffffe3, 28),
    (0xfffffe4, 28),  (0xfffffe5, 28),  (0xfffffe6, 28),  (0xfffffe7, 28),
    (0xfffffe8, 28),  (0xffffea, 24),   (0x3ffffffc, 30), (0xfffffe9, 28),
    (0xfffffea, 28),  (0x3ffffffd, 30), (0xfffffeb, 28),  (0xfffffec, 28),
    (0xfffffed, 28),  (0xfffffee, 28),  (0xfffffef, 28),  (0xffffff0, 28),
    (0xffffff1, 28),  (0xffffff2, 28),  (0x3ffffffe, 30), (0xffffff3, 28),
    (0xffffff4, 28),  (0xffffff5, 28),  (0xffffff6, 28),  (0xffffff7, 28),
    (0xffffff8, 28),  (0xffffff9, 28),  (0xffffffa, 28),  (0xffffffb, 28),
    // 32 ' ' .. 63 '?'
    (0x14, 6),        (0x3f8, 10),      (0x3f9, 10),      (0xffa, 12),
    (0x1ff9, 13),     (0x15, 6),        (0xf8, 8),        (0x7fa, 11),
    (0x3fa, 10),      (0x3fb, 10),      (0xf9, 8),        (0x7fb, 11),
    (0xfa, 8),        (0x16, 6),        (0x17, 6),        (0x18, 6),
    (0x0, 5),         (0x1, 5),         (0x2, 5),         (0x19, 6),
    (0x1a, 6),        (0x1b, 6),        (0x1c, 6),        (0x1d, 6),
    (0x1e, 6),        (0x1f, 6),        (0x5c, 7),        (0xfb, 8),
    (0x7ffc, 15),     (0x20, 6),        (0xffb, 12),      (0x3fc, 10),
    // 64 '@' .. 95 '_'
    (0x1ffa, 13),     (0x21, 6),        (0x5d, 7),        (0x5e, 7),
    (0x5f, 7),        (0x60, 7),        (0x61, 7),        (0x62, 7),
    (0x63, 7),        (0x64, 7),        (0x65, 7),        (0x66, 7),
    (0x67, 7),        (0x68, 7),        (0x69, 7),        (0x6a, 7),
    (0x6b, 7),        (0x6c, 7),        (0x6d, 7),        (0x6e, 7),
    (0x6f, 7),        (0x70, 7),        (0x71, 7),        (0x72, 7),
    (0xfc, 8),        (0x73, 7),        (0xfd, 8),        (0x1ffb, 13),
    (0x7fff0, 19),    (0x1ffc, 13),     (0x3ffc, 14),     (0x22, 6),
    // 96 '`' .. 127
    (0x7ffd, 15),     (0x3, 5),         (0x23, 6),        (0x4, 5),
    (0x24, 6),        (0x5, 5),         (0x25, 6),        (0x26, 6),
    (0x27, 6),        (0x6, 5),         (0x74, 7),        (0x75, 7),
    (0x28, 6),        (0x29, 6),        (0x2a, 6),        (0x7, 5),
    (0x2b, 6),        (0x76, 7),        (0x2c, 6),        (0x8, 5),
    (0x9, 5),         (0x2d, 6),        (0x77, 7),        (0x78, 7),
    (0x79, 7),        (0x7a, 7),        (0x7b, 7),        (0x7fffe, 19),
    (0x7fc, 11),      (0x3ffd, 14),     (0x1ffd, 13),     (0xffffffc, 28),
    // 128 .. 159
    (0xfffe6, 20),    (0x3fffd2, 22),   (0xfffe7, 20),    (0xfffe8, 20),
    (0x3fffd3, 22),   (0x3fffd4, 22),   (0x3fffd5, 22),   (0x7fffd9, 23),
    (0x3fffd6, 22),   (0x7fffda, 23),   (0x7fffdb, 23),   (0x7fffdc, 23),
    (0x7fffdd, 23),   (0x7fffde, 23),   (0xffffeb, 24),   (0x7fffdf, 23),
    (0xffffec, 24),   (0xffffed, 24),   (0x3fffd7, 22),   (0x7fffe0, 23),
    (0xffffee, 24),   (0x7fffe1, 23),   (0x7fffe2, 23),   (0x7fffe3, 23),
    (0x7fffe4, 23),   (0x1fffdc, 21),   (0x3fffd8, 22),   (0x7fffe5, 23),
    (0x3fffd9, 22),   (0x7fffe6, 23),   (0x7fffe7, 23),   (0xffffef, 24),
    // 160 .. 191
    (0x3fffda, 22),   (0x1fffdd, 21),   (0xfffe9, 20),    (0x3fffdb, 22),
    (0x3fffdc, 22),   (0x7fffe8, 23),   (0x7fffe9, 23),   (0x1fffde, 21),
    (0x7fffea, 23),   (0x3fffdd, 22),   (0x3fffde, 22),   (0xfffff0, 24),
    (0x1fffdf, 21),   (0x3fffdf, 22),   (0x7fffeb, 23),   (0x7fffec, 23),
    (0x1fffe0, 21),   (0x1fffe1, 21),   (0x3fffe0, 22),   (0x1fffe2, 21),
    (0x7fffed, 23),   (0x3fffe1, 22),   (0x7fffee, 23),   (0x7fffef, 23),
    (0xfffea, 20),    (0x3fffe2, 22),   (0x3fffe3, 22),   (0x3fffe4, 22),
    (0x7ffff0, 23),   (0x3fffe5, 22),   (0x3fffe6, 22),   (0x7ffff1, 23),
    // 192 .. 223
    (0x3ffffe0, 26),  (0x3ffffe1, 26),  (0xfffeb, 20),    (0x7fff1, 19),
    (0x3fffe7, 22),   (0x7ffff2, 23),   (0x3fffe8, 22),   (0x1ffffec, 25),
    (0x3ffffe2, 26),  (0x3ffffe3, 26),  (0x3ffffe4, 26),  (0x7ffffde, 27),
    (0x7ffffdf, 27),  (0x3ffffe5, 26),  (0xfffff1, 24),   (0x1ffffed, 25),
    (0x7fff2, 19),    (0x1fffe3, 21),   (0x3ffffe6, 26),  (0x7ffffe0, 27),
    (0x7ffffe1, 27),  (0x3ffffe7, 26),  (0x7ffffe2, 27),  (0xfffff2, 24),
    (0x1fffe4, 21),   (0x1fffe5, 21),   (0x3ffffe8, 26),  (0x3ffffe9, 26),
    (0xffffffd, 28),  (0x7ffffe3, 27),  (0x7ffffe4, 27),  (0x7ffffe5, 27),
    // 224 .. 255
    (0xfffec, 20),    (0xfffff3, 24),   (0xfffed, 20),    (0x1fffe6, 21),
    (0x3fffe9, 22),   (0x1fffe7, 21),   (0x1fffe8, 21),   (0x7ffff3, 23),
    (0x3fffea, 22),   (0x3fffeb, 22),   (0x1ffffee, 25),  (0x1ffffef, 25),
    (0xfffff4, 24),   (0xfffff5, 24),   (0x3ffffea, 26),  (0x7ffff4, 23),
    (0x3ffffeb, 26),  (0x7ffffe6, 27),  (0x3ffffec, 26),  (0x3ffffed, 26),
    (0x7ffffe7, 27),  (0x7ffffe8, 27),  (0x7ffffe9, 27),  (0x7ffffea, 27),
    (0x7ffffeb, 27),  (0xffffffe, 28),  (0x7ffffec, 27),  (0x7ffffed, 27),
    (0x7ffffee, 27),  (0x7ffffef, 27),  (0x7fffff0, 27),  (0x3ffffee, 26),
    // 256 EOS
    (0x3fffffff, 30),
];

// ---------------------------------------------------------------------------
// Huffman binary-tree decoder (built once via LazyLock)
// ---------------------------------------------------------------------------

struct HuffmanNode {
    /// `None` = internal node, `Some(sym)` = leaf.
    symbol: Option<u16>,
    /// children[0] = 0-bit, children[1] = 1-bit. 0 means "no child".
    children: [u32; 2],
}

struct HuffmanTree {
    nodes: Vec<HuffmanNode>,
}

impl HuffmanTree {
    fn build() -> Self {
        // Pre-allocate generously (max depth 30, but tree is sparse).
        let mut nodes = Vec::with_capacity(1024);
        nodes.push(HuffmanNode {
            symbol: None,
            children: [0, 0],
        });

        for (sym, &(code, len)) in HUFFMAN_TABLE.iter().enumerate() {
            let mut idx: usize = 0;
            for bit_pos in (0..len).rev() {
                let bit = ((code >> bit_pos) & 1) as usize;
                if nodes[idx].children[bit] == 0 {
                    let new_idx = nodes.len() as u32;
                    nodes.push(HuffmanNode {
                        symbol: None,
                        children: [0, 0],
                    });
                    nodes[idx].children[bit] = new_idx;
                }
                idx = nodes[idx].children[bit] as usize;
            }
            nodes[idx].symbol = Some(sym as u16);
        }

        HuffmanTree { nodes }
    }

    fn decode(&self, data: &[u8]) -> Result<Vec<u8>, &'static str> {
        let mut result = Vec::with_capacity(data.len() * 2);
        let mut node_idx: usize = 0;
        let mut bits_since_last_sym: u8 = 0;

        for &byte in data {
            for shift in (0..8).rev() {
                let bit = ((byte >> shift) & 1) as usize;
                let child = self.nodes[node_idx].children[bit] as usize;
                if child == 0 {
                    return Err("invalid huffman code");
                }
                node_idx = child;
                bits_since_last_sym += 1;

                if let Some(sym) = self.nodes[node_idx].symbol {
                    if sym == 256 {
                        // EOS symbol encountered in data — invalid per RFC 7541 §5.2.
                        return Err("unexpected EOS in huffman stream");
                    }
                    result.push(sym as u8);
                    node_idx = 0;
                    bits_since_last_sym = 0;
                }
            }
        }

        // Remaining bits must be ≤7 and all-ones (EOS prefix padding).
        if bits_since_last_sym > 7 {
            return Err("invalid huffman padding length");
        }

        Ok(result)
    }
}

static TREE: LazyLock<HuffmanTree> = LazyLock::new(HuffmanTree::build);

fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    TREE.decode(data)
}

// ---------------------------------------------------------------------------
// HPACK primitive decoders
// ---------------------------------------------------------------------------

/// Decode an HPACK integer with the given prefix width (RFC 7541 §5.1).
/// Returns `(value, bytes_consumed)`.
fn decode_integer(buf: &[u8], prefix_bits: u8) -> Result<(usize, usize), &'static str> {
    if buf.is_empty() {
        return Err("empty buffer for integer decode");
    }
    let prefix_mask = (1u16 << prefix_bits) - 1;
    let mut value = (buf[0] & prefix_mask as u8) as usize;
    if value < prefix_mask as usize {
        return Ok((value, 1));
    }
    let mut m: u32 = 0;
    let mut pos = 1;
    loop {
        if pos >= buf.len() {
            return Err("truncated hpack integer");
        }
        let b = buf[pos];
        value += ((b & 0x7F) as usize) << m;
        pos += 1;
        if b & 0x80 == 0 {
            return Ok((value, pos));
        }
        m += 7;
        if m > 28 {
            return Err("hpack integer overflow");
        }
    }
}

/// Decode an HPACK string literal (RFC 7541 §5.2).
/// Returns `(decoded_bytes, total_bytes_consumed_from_buf)`.
fn decode_string(buf: &[u8]) -> Result<(Vec<u8>, usize), &'static str> {
    if buf.is_empty() {
        return Err("empty buffer for string decode");
    }
    let is_huffman = buf[0] & 0x80 != 0;
    let (length, int_bytes) = decode_integer(buf, 7)?;
    let start = int_bytes;
    let end = start + length;
    if end > buf.len() {
        return Err("truncated hpack string");
    }
    let raw = &buf[start..end];
    let decoded = if is_huffman {
        huffman_decode(raw)?
    } else {
        raw.to_vec()
    };
    Ok((decoded, end))
}

/// Skip an HPACK string literal without decoding its value.
fn skip_string(buf: &[u8]) -> Result<usize, &'static str> {
    if buf.is_empty() {
        return Err("empty buffer for string skip");
    }
    let (length, int_bytes) = decode_integer(buf, 7)?;
    let total = int_bytes + length;
    if total > buf.len() {
        return Err("truncated hpack string");
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Public API: extract `:path` from an HPACK-encoded header block.
// ---------------------------------------------------------------------------

/// Scan an HPACK header block and return the value of the `:path` pseudo-header.
///
/// Only handles the static table — the dynamic table is empty on the first
/// HEADERS frame of a fresh connection, which is the only frame we inspect.
pub fn find_path_header(header_block: &[u8]) -> Result<String, &'static str> {
    let mut pos = 0;
    while pos < header_block.len() {
        let byte = header_block[pos];

        if byte & 0x80 != 0 {
            // §6.1 — Indexed Header Field
            let (index, consumed) = decode_integer(&header_block[pos..], 7)?;
            pos += consumed;
            if index == 4 {
                return Ok("/".to_string());
            }
            if index == 5 {
                return Ok("/index.html".to_string());
            }
        } else if byte & 0xC0 == 0x40 {
            // §6.2.1 — Literal with Incremental Indexing
            let (name_index, consumed) = decode_integer(&header_block[pos..], 6)?;
            pos += consumed;
            if name_index == 0 {
                pos += skip_string(&header_block[pos..])?;
            }
            if name_index == PATH_INDEX {
                let (value, _) = decode_string(&header_block[pos..])?;
                return String::from_utf8(value).map_err(|_| "path is not valid utf-8");
            } else {
                pos += skip_string(&header_block[pos..])?;
            }
        } else if byte & 0xF0 == 0x00 || byte & 0xF0 == 0x10 {
            // §6.2.2 — Literal without Indexing  /  §6.2.3 — Literal Never Indexed
            let (name_index, consumed) = decode_integer(&header_block[pos..], 4)?;
            pos += consumed;
            if name_index == 0 {
                pos += skip_string(&header_block[pos..])?;
            }
            if name_index == PATH_INDEX {
                let (value, _) = decode_string(&header_block[pos..])?;
                return String::from_utf8(value).map_err(|_| "path is not valid utf-8");
            } else {
                pos += skip_string(&header_block[pos..])?;
            }
        } else if byte & 0xE0 == 0x20 {
            // §6.3 — Dynamic Table Size Update
            let (_, consumed) = decode_integer(&header_block[pos..], 5)?;
            pos += consumed;
        } else {
            return Err("unrecognised hpack field encoding");
        }
    }

    Err(":path header not found in header block")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Huffman encode helper (for building test vectors) -------------------

    fn huffman_encode(input: &[u8]) -> Vec<u8> {
        let mut bits: u64 = 0;
        let mut bit_count: u32 = 0;
        let mut out = Vec::new();

        for &byte in input {
            let (code, len) = HUFFMAN_TABLE[byte as usize];
            bits = (bits << len) | code as u64;
            bit_count += len as u32;
            while bit_count >= 8 {
                bit_count -= 8;
                out.push((bits >> bit_count) as u8);
                bits &= (1u64 << bit_count) - 1;
            }
        }
        // Pad remaining bits with 1s (EOS prefix).
        if bit_count > 0 {
            let pad = 8 - bit_count;
            bits = (bits << pad) | ((1u64 << pad) - 1);
            out.push(bits as u8);
        }
        out
    }

    /// Build an HPACK string literal (§5.2).
    fn hpack_string(value: &[u8], use_huffman: bool) -> Vec<u8> {
        let encoded = if use_huffman {
            huffman_encode(value)
        } else {
            value.to_vec()
        };
        let mut out = Vec::new();
        let len = encoded.len();
        assert!(len < 127, "test helper only handles short strings");
        out.push(if use_huffman { 0x80 | len as u8 } else { len as u8 });
        out.extend_from_slice(&encoded);
        out
    }

    // -- decode_integer -----------------------------------------------------

    #[test]
    fn integer_single_byte() {
        // Value 10 with 5-bit prefix: fits in one byte (10 < 31).
        let buf = [0b0000_1010];
        let (val, consumed) = decode_integer(&buf, 5).unwrap();
        assert_eq!(val, 10);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn integer_multi_byte() {
        // RFC 7541 §C.1.3: encoding 1337 with 5-bit prefix.
        // 31 + 1306 = 1337.  1306 = 0x51a
        // Encoding: prefix byte 0x1f, then 0x9a (154-128=26, continue), 0x0a (10, stop).
        // 26 + 10*128 = 26 + 1280 = 1306. 1306 + 31 = 1337. ✓
        let buf = [0x1f, 0x9a, 0x0a];
        let (val, consumed) = decode_integer(&buf, 5).unwrap();
        assert_eq!(val, 1337);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn integer_with_6bit_prefix() {
        // Value 4 with 6-bit prefix: 0x44 & 0x3f = 4.
        let buf = [0x44];
        let (val, consumed) = decode_integer(&buf, 6).unwrap();
        assert_eq!(val, 4);
        assert_eq!(consumed, 1);
    }

    // -- huffman_decode / huffman_encode roundtrip ---------------------------

    #[test]
    fn huffman_roundtrip_ascii() {
        let input = b"/mypackage.MyService/DoThing";
        let encoded = huffman_encode(input);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn huffman_roundtrip_short() {
        let input = b"/";
        let encoded = huffman_encode(input);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn huffman_roundtrip_www_example_com() {
        // RFC 7541 §C.4.1 encodes "www.example.com" as:
        let expected_encoded: &[u8] = &[
            0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
        ];
        let encoded = huffman_encode(b"www.example.com");
        assert_eq!(encoded, expected_encoded);
        let decoded = huffman_decode(expected_encoded).unwrap();
        assert_eq!(decoded, b"www.example.com");
    }

    // -- decode_string ------------------------------------------------------

    #[test]
    fn string_plain() {
        let mut buf = vec![0x05]; // length 5, no Huffman
        buf.extend_from_slice(b"hello");
        let (val, consumed) = decode_string(&buf).unwrap();
        assert_eq!(val, b"hello");
        assert_eq!(consumed, 6);
    }

    #[test]
    fn string_huffman() {
        let encoded_str = huffman_encode(b"/test");
        let mut buf = vec![0x80 | encoded_str.len() as u8]; // Huffman flag set
        buf.extend_from_slice(&encoded_str);
        let (val, consumed) = decode_string(&buf).unwrap();
        assert_eq!(val, b"/test");
        assert_eq!(consumed, buf.len());
    }

    // -- find_path_header ---------------------------------------------------

    #[test]
    fn path_from_indexed_static_4() {
        // Indexed representation: index 4 = :path /
        let block = [0x84]; // 0x80 | 4
        assert_eq!(find_path_header(&block).unwrap(), "/");
    }

    #[test]
    fn path_from_indexed_static_5() {
        // Indexed representation: index 5 = :path /index.html
        let block = [0x85]; // 0x80 | 5
        assert_eq!(find_path_header(&block).unwrap(), "/index.html");
    }

    #[test]
    fn path_literal_incremental_plain() {
        // :method POST (indexed 3), then :path literal incremental with plain string
        let path = b"/pkg.Svc/Method";
        let mut block = vec![0x83]; // indexed :method POST
        block.push(0x44); // literal incremental, name index = 4 (:path)
        block.push(path.len() as u8); // plain string length
        block.extend_from_slice(path);
        assert_eq!(find_path_header(&block).unwrap(), "/pkg.Svc/Method");
    }

    #[test]
    fn path_literal_incremental_huffman() {
        let path = b"/mypackage.MyService/DoThing";
        let mut block = vec![0x83]; // indexed :method POST
        block.push(0x44); // literal incremental, name index = 4
        block.extend_from_slice(&hpack_string(path, true));
        assert_eq!(find_path_header(&block).unwrap(), "/mypackage.MyService/DoThing");
    }

    #[test]
    fn path_literal_without_indexing() {
        // §6.2.2: 0000xxxx prefix, name index = 4
        let path = b"/no-index/path";
        let mut block = vec![0x04]; // literal without indexing, name index = 4
        block.extend_from_slice(&hpack_string(path, false));
        assert_eq!(find_path_header(&block).unwrap(), "/no-index/path");
    }

    #[test]
    fn path_literal_never_indexed() {
        // §6.2.3: 0001xxxx prefix, name index = 4
        let path = b"/never-indexed/path";
        let mut block = vec![0x14]; // literal never indexed, name index = 4
        block.extend_from_slice(&hpack_string(path, false));
        assert_eq!(find_path_header(&block).unwrap(), "/never-indexed/path");
    }

    #[test]
    fn path_not_found() {
        // Header block with :method and :scheme but no :path.
        let block = [0x82, 0x86]; // :method GET, :scheme http
        assert!(find_path_header(&block).is_err());
    }

    #[test]
    fn path_after_dynamic_table_size_update() {
        // §6.3 dynamic table size update (001xxxxx), new max = 0
        let mut block = vec![0x20]; // size update, new max = 0
        block.push(0x84); // indexed :path /
        assert_eq!(find_path_header(&block).unwrap(), "/");
    }

    #[test]
    fn rfc_c41_first_request() {
        // RFC 7541 §C.4.1 — first request on the connection.
        let block: &[u8] = &[
            0x82, 0x86, 0x84, 0x41, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab,
            0x90, 0xf4, 0xff,
        ];
        assert_eq!(find_path_header(block).unwrap(), "/");
    }
}
