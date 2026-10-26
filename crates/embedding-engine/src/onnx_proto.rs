//! Minimal read-only reader for the *declared graph input shapes* of an ONNX
//! file (enough to synthesize the `past_key_values` tensors that decoder
//! exports with a KV cache declare).
//!
//! The ONNX format is a plain protobuf (see `onnx.proto`). We walk
//! `ModelProto.graph` (field 7) -> `GraphProto.input` (field 11 in the
//! current proto, field 5 in legacy exports) -> `ValueInfoProto`
//! (`name` = 1, `type` = 2) -> `TypeProto.tensor_type` (field 1).
//! No third-party dependency is introduced; the walker only supports the
//! wire types these messages use (varint, length-delimited) and skips
//! fixed-width fields.
//!
//! The tensor shape is parsed schema-agnostically: recent `onnx` versions
//! nest the shape as `Tensor{elem_type=1, shape=2}` with
//! `TensorShapeProto{dim=1}` and `Dimension{dim_value=1, dim_param=2}`,
//! whereas legacy exports use `TensorShapeProto{elem_type=1, dim=2}` with
//! `Dimension{dim_param=1, dim_value=2}`. A `Dimension` therefore reads its
//! static value from *either* field 1 or field 2 (a varint = static value,
//! otherwise a dynamic/symbolic dimension = `None`), and the dimension list
//! is taken from the shape wrapper's `dim` field when present.

/// ONNX `TensorProto.DataType` values we may encounter.
pub const TENSOR_FLOAT: u32 = 1;
#[allow(dead_code)]
pub const TENSOR_INT64: u32 = 7;

/// One declared graph input with its static dimension values (dynamic /
/// symbolic dimensions are `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputShape {
    /// Input name as declared by the graph (e.g. `past_key_values.0.key`).
    pub name: String,
    /// `TensorProto.DataType` of the tensor.
    pub elem_type: u32,
    /// Dimensions in rank order; `None` = dynamic (symbolic) dimension.
    pub dims: Vec<Option<i64>>,
}

/// A single protobuf field as decoded by [`Reader::fields`].
#[allow(dead_code)]
enum Field<'a> {
    /// Wire type 0 (varint); value kept for callers that need it.
    Varint(i64),
    /// Wire type 2 (length-delimited), e.g. strings and nested messages.
    Bytes(&'a [u8]),
    /// Fixed-width field (wire types 1/5), value not needed.
    Skipped,
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn read_varint(&mut self) -> Result<i64, String> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *self
                .b
                .get(self.pos)
                .ok_or_else(|| "onnx: truncated varint".to_string())?;
            self.pos += 1;
            result |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(result as i64);
            }
            shift += 7;
            if shift >= 70 {
                return Err("onnx: varint overflow".to_string());
            }
        }
    }

    fn skip_bytes(&mut self, n: usize) -> Result<(), String> {
        if self.pos + n > self.b.len() {
            return Err("onnx: truncated field".to_string());
        }
        self.pos += n;
        Ok(())
    }

    /// Decode all fields of the message blob held by this reader.
    fn fields(&self) -> Result<Vec<(u32, Field<'a>)>, String> {
        let mut out = Vec::new();
        let mut r = Self::new(self.b);
        while r.pos < r.b.len() {
            let tag = r.read_varint()?;
            let (field, wire_type) = ((tag >> 3) as u32, (tag & 7) as u32);
            match wire_type {
                0 => out.push((field, Field::Varint(r.read_varint()?))),
                1 => {
                    r.skip_bytes(8)?;
                    out.push((field, Field::Skipped));
                }
                2 => {
                    let len = r.read_varint()? as usize;
                    let start = r.pos;
                    r.skip_bytes(len)?;
                    out.push((field, Field::Bytes(&r.b[start..start + len])));
                }
                5 => {
                    r.skip_bytes(4)?;
                    out.push((field, Field::Skipped));
                }
                other => return Err(format!("onnx: unsupported wire type {other}")),
            }
        }
        Ok(out)
    }
}

/// Interpret one `Dimension` blob: a varint at field 1 or 2 is a static
/// `dim_value` (`Some(Some(v))`); a symbolic dimension is `Some(None)`;
/// `None` means the blob is not a parseable dimension. Schema agnostic
/// (current and legacy `Dimension` field numbering).
fn parse_dim(blob: &[u8]) -> Option<Option<i64>> {
    let fields = Reader::new(blob).fields().ok()?;
    for (f, p) in fields {
        if f == 1 || f == 2 {
            if let Field::Varint(v) = p {
                return Some(Some(v));
            }
        }
    }
    Some(None)
}

/// Read the declared graph inputs (names, element types, static dims) of the
/// ONNX model at `model_path`.
pub fn read_input_shapes(model_path: &str) -> Result<Vec<InputShape>, String> {
    let data = std::fs::read(model_path).map_err(|e| format!("read onnx file: {e}"))?;
    let graph = Reader::new(&data)
        .fields()?
        .into_iter()
        .find(|(f, p)| *f == 7 && matches!(p, Field::Bytes(_)))
        .and_then(|(_, p)| match p {
            Field::Bytes(b) => Some(b),
            _ => None,
        })
        .ok_or_else(|| "onnx: no graph (ModelProto field 7)".to_string())?;

    // GraphProto.input is field 11 (current proto) or field 5 (legacy).
    let gfields = Reader::new(graph).fields()?;
    let input_blobs: Vec<&[u8]> = gfields
        .iter()
        .filter(|(f, p)| (*f == 11 || *f == 5) && matches!(p, Field::Bytes(_)))
        .map(|(_, p)| match p {
            Field::Bytes(b) => *b,
            _ => unreachable!(),
        })
        .collect();

    let mut out = Vec::new();
    for vi in input_blobs {
        let mut name = String::new();
        let mut tensor_type: Option<&[u8]> = None;
        for (f, p) in Reader::new(vi).fields()? {
            match f {
                1 => {
                    if let Field::Bytes(bytes) = p {
                        name = String::from_utf8_lossy(bytes).into_owned();
                    }
                }
                2 => {
                    tensor_type = match p {
                        Field::Bytes(b) => Some(b),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        let Some(tt) = tensor_type else {
            continue;
        };
        // `TypeProto.tensor_type` (field 1) is the `Tensor` message holding
        // `elem_type` (1) and `shape` (2).
        let Some(tensor) = Reader::new(tt)
            .fields()?
            .into_iter()
            .find(|(f, p)| *f == 1 && matches!(p, Field::Bytes(_)))
            .and_then(|(_, p)| match p {
                Field::Bytes(b) => Some(b),
                _ => None,
            })
        else {
            continue;
        };
        let mut elem_type = 0u32;
        let mut shape_blobs: Vec<&[u8]> = Vec::new();
        for (f, p) in Reader::new(tensor).fields()? {
            match f {
                1 => {
                    if let Field::Varint(v) = p {
                        elem_type = v as u32;
                    }
                }
                2 => {
                    if let Field::Bytes(b) = p {
                        shape_blobs.push(b);
                    }
                }
                _ => {}
            }
        }
        // Current schema: a single field-2 blob is the `shape` wrapper whose
        // `dim` (field 1) entries are the dimensions. Legacy schema: the
        // field-2 blobs of the tensor type *are* the dimensions.
        let mut dims: Vec<Option<i64>> = Vec::new();
        for shape in &shape_blobs {
            let sub = Reader::new(shape).fields()?;
            let dim_entries: Vec<&[u8]> = sub
                .iter()
                .filter(|(f, p)| *f == 1 && matches!(p, Field::Bytes(_)))
                .map(|(_, p)| match p {
                    Field::Bytes(b) => *b,
                    _ => unreachable!(),
                })
                .collect();
            if !dim_entries.is_empty() {
                for d in dim_entries {
                    if let Some(dim) = parse_dim(d) {
                        dims.push(dim);
                    }
                }
            } else {
                // No nested `dim` list: the blob is itself a legacy dimension.
                if let Some(dim) = parse_dim(shape) {
                    dims.push(dim);
                }
            }
        }
        out.push(InputShape {
            name,
            elem_type,
            dims,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path of the checked-in synthetic fixture (serialized by the reference
    /// `onnx` package): a ModelProto with graph inputs `input_ids`
    /// (int64 [batch_size, sequence_length]), `attention_mask`
    /// (int64 [batch_size, total_seq]), and `past_key_values.0.{key,value}`
    /// (f32 [batch_size, 8, past_seq, 128]).
    fn fixture_path() -> Option<std::path::PathBuf> {
        let root = std::env::var_os("CARGO_MANIFEST_DIR")?;
        let p = std::path::Path::new(&root).join("tests/fixtures/onnx_proto_fixture.onnx");
        p.exists().then_some(p)
    }

    #[test]
    fn synthetic_fixture_parses_names_types_and_static_dims() {
        let Some(path) = fixture_path() else {
            eprintln!("SKIP synthetic_fixture (fixture file absent)");
            return;
        };
        let shapes = read_input_shapes(path.to_str().unwrap()).expect("fixture must parse");
        assert_eq!(shapes.len(), 4);
        let ids = &shapes[0];
        assert_eq!(ids.name, "input_ids");
        assert_eq!(ids.elem_type, TENSOR_INT64);
        assert_eq!(ids.dims, vec![None, None]);
        let mask = &shapes[1];
        assert_eq!(mask.name, "attention_mask");
        assert_eq!(mask.elem_type, TENSOR_INT64);
        let kv = &shapes[2];
        assert_eq!(kv.name, "past_key_values.0.key");
        assert_eq!(kv.elem_type, TENSOR_FLOAT);
        assert_eq!(kv.dims, vec![None, Some(8), None, Some(128)]);
        let kvv = &shapes[3];
        assert_eq!(kvv.name, "past_key_values.0.value");
        assert_eq!(kvv.dims, vec![None, Some(8), None, Some(128)]);
    }

    #[test]
    fn missing_file_is_a_parse_error() {
        assert!(read_input_shapes("does-not-exist.onnx").is_err());
    }
}
