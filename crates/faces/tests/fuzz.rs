//! Fuzz target for the in-house ONNX reader (protobuf) and the graph/net construction on top of it.
//! Short runs in every `cargo test`; long runs: `scripts/fuzz.sh`.

#[global_allocator]
static ALLOC: fuzzkit::CapAlloc = fuzzkit::CapAlloc;

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn field(out: &mut Vec<u8>, f: u32, body: &[u8]) {
    varint(out, ((f as u64) << 3) | 2);
    varint(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn field_int(out: &mut Vec<u8>, f: u32, v: u64) {
    varint(out, (f as u64) << 3);
    varint(out, v);
}

/// Small model: Conv with weights and bias, Relu, Reshape with a constant shape.
fn small_model() -> Vec<u8> {
    let mut g = Vec::new();
    // Conv node with attributes
    let mut n = Vec::new();
    for i in ["x", "w", "b"] {
        field(&mut n, 1, i.as_bytes());
    }
    field(&mut n, 2, b"c");
    field(&mut n, 4, b"Conv");
    let mut a = Vec::new();
    field(&mut a, 1, b"kernel_shape");
    let mut ints = Vec::new();
    varint(&mut ints, 1);
    varint(&mut ints, 1);
    field(&mut a, 8, &ints);
    field_int(&mut a, 20, 7);
    field(&mut n, 5, &a);
    field(&mut g, 1, &n);
    let mut n = Vec::new();
    field(&mut n, 1, b"c");
    field(&mut n, 2, b"y");
    field(&mut n, 4, b"Relu");
    field(&mut g, 1, &n);
    let mut n = Vec::new();
    field(&mut n, 1, b"y");
    field(&mut n, 1, b"s");
    field(&mut n, 2, b"z");
    field(&mut n, 4, b"Reshape");
    field(&mut g, 1, &n);
    // initializers
    let tensor = |name: &str, dims: &[u64], ty: u64, raw: &[u8]| {
        let mut t = Vec::new();
        for d in dims {
            field_int(&mut t, 1, *d);
        }
        field_int(&mut t, 2, ty);
        field(&mut t, 8, name.as_bytes());
        field(&mut t, 9, raw);
        t
    };
    let w: Vec<u8> = [0.5f32, -0.25].iter().flat_map(|v| v.to_le_bytes()).collect();
    field(&mut g, 5, &tensor("w", &[2, 1, 1, 1], 1, &w));
    field(&mut g, 5, &tensor("b", &[2], 1, &[0; 8]));
    let s: Vec<u8> = [1i64, -1].iter().flat_map(|v| v.to_le_bytes()).collect();
    field(&mut g, 5, &tensor("s", &[2], 7, &s));
    let mut vi = Vec::new();
    field(&mut vi, 1, b"x");
    field(&mut g, 11, &vi);
    let mut vo = Vec::new();
    field(&mut vo, 1, b"z");
    field(&mut g, 12, &vo);
    let mut m = Vec::new();
    field_int(&mut m, 1, 7);
    field(&mut m, 7, &g);
    m
}

fn exercise(d: &[u8]) {
    if let Ok(m) = faces::onnx::parse(d) {
        let _ = faces::graph::Graph::new(m);
    }
    if let Ok(m) = faces::onnx::parse(d) {
        let _ = faces::net::Net::new(m);
    }
}

#[test]
fn seed_parses() {
    let m = faces::onnx::parse(&small_model()).unwrap();
    assert_eq!(m.nodes.len(), 3);
    assert_eq!(m.weights.len(), 3);
}

#[test]
fn fuzz_onnx_small() {
    fuzzkit::run("onnx_small", &[small_model()], 1 << 12, exercise);
}

/// The bundled YuNet model as seed (only the reader: building the graph of the big model per input
/// would make the short run slow).
#[test]
fn fuzz_onnx_yunet() {
    let iters = std::env::var("GRAINEO_FUZZ_ITERS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(400);
    if iters < 100 {
        return;
    }
    fuzzkit::run("onnx_yunet", &[faces::yunet::MODEL.to_vec()], 1 << 19, |d| {
        let _ = faces::onnx::parse(d);
    });
}
