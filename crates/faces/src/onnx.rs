//! Minimal ONNX reader (hand-written protobuf): only what is needed to run small face networks –
//! nodes (operator, inputs/outputs, attributes) and weights (initializers).

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Tensor {
    pub dims: Vec<usize>,
    pub data: Vec<f32>,
}

#[derive(Debug, Clone)]
pub enum Attr {
    Int(i64),
    Float(f32),
    Ints(Vec<i64>),
    Floats(Vec<f32>),
    Text(String),
    Tensor(Tensor),
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub op: String,
    pub name: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attr: HashMap<String, Attr>,
}

impl Node {
    pub fn ints(&self, name: &str) -> Option<&[i64]> {
        match self.attr.get(name) {
            Some(Attr::Ints(v)) => Some(v),
            _ => None,
        }
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.attr.get(name) {
            Some(Attr::Int(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn float(&self, name: &str) -> Option<f32> {
        match self.attr.get(name) {
            Some(Attr::Float(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.attr.get(name) {
            Some(Attr::Text(v)) => Some(v),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub weights: HashMap<String, Tensor>,
    pub graph_inputs: Vec<String>,
    pub graph_outputs: Vec<String>,
}

struct ProtoReader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> ProtoReader<'a> {
    fn at_end(&self) -> bool {
        self.p >= self.b.len()
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        let mut s = 0;
        loop {
            let byte = *self.b.get(self.p).ok_or("Protobuf: Ende erreicht")?;
            self.p += 1;
            v |= ((byte & 0x7f) as u64) << s;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            s += 7;
            if s > 63 {
                return Err("Protobuf: Zahl zu lang".into());
            }
        }
    }
    /// (field number, wire type)
    fn tag(&mut self) -> Result<(u32, u8), String> {
        let k = self.varint()?;
        Ok(((k >> 3) as u32, (k & 7) as u8))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.varint()? as usize;
        let e = self.p.checked_add(n).filter(|e| *e <= self.b.len()).ok_or("Protobuf: Länge außerhalb")?;
        let s = &self.b[self.p..e];
        self.p = e;
        Ok(s)
    }
    fn fix32(&mut self) -> Result<[u8; 4], String> {
        let s = self.b.get(self.p..self.p + 4).ok_or("Protobuf: Ende erreicht")?;
        self.p += 4;
        Ok([s[0], s[1], s[2], s[3]])
    }
    fn skip_field(&mut self, wire: u8) -> Result<(), String> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.p += 8,
            2 => {
                self.bytes()?;
            }
            5 => self.p += 4,
            w => return Err(format!("Protobuf: Wire-Typ {w}")),
        }
        Ok(())
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn tensor(b: &[u8]) -> Result<(String, Tensor), String> {
    let mut l = ProtoReader { b, p: 0 };
    let (mut name, mut type_name) = (String::new(), 1u64);
    let mut dims: Vec<usize> = Vec::new();
    let (mut raw, mut fl, mut i32s, mut i64s): (Option<&[u8]>, Vec<f32>, Vec<i32>, Vec<i64>) = (None, Vec::new(), Vec::new(), Vec::new());
    while !l.at_end() {
        let (f, w) = l.tag()?;
        match (f, w) {
            (1, 0) => dims.push(l.varint()? as usize),
            (1, 2) => {
                let mut s = ProtoReader { b: l.bytes()?, p: 0 };
                while !s.at_end() {
                    dims.push(s.varint()? as usize);
                }
            }
            (2, 0) => type_name = l.varint()?,
            (4, 2) => {
                let s = l.bytes()?;
                fl.extend(s.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
            }
            (4, 5) => fl.push(f32::from_le_bytes(l.fix32()?)),
            (5, 2) => {
                let mut s = ProtoReader { b: l.bytes()?, p: 0 };
                while !s.at_end() {
                    i32s.push(s.varint()? as i64 as i32);
                }
            }
            (7, 2) => {
                let mut s = ProtoReader { b: l.bytes()?, p: 0 };
                while !s.at_end() {
                    i64s.push(s.varint()? as i64);
                }
            }
            (8, 2) => name = text(l.bytes()?),
            (9, 2) => raw = Some(l.bytes()?),
            (_, w) => l.skip_field(w)?,
        }
    }
    let data: Vec<f32> = match (type_name, raw) {
        (1, Some(r)) => r.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        (1, None) => fl,
        (7, Some(r)) => r.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap()) as f32).collect(),
        (7, None) => i64s.iter().map(|v| *v as f32).collect(),
        (6, Some(r)) => r.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32).collect(),
        (6, None) => i32s.iter().map(|v| *v as f32).collect(),
        (t, _) => return Err(format!("Tensor {name}: Datentyp {t} nicht unterstützt")),
    };
    // the shape must describe exactly the data: every later index runs over `dims`
    let expected = dims.iter().try_fold(1usize, |a, d| a.checked_mul(*d));
    if expected != Some(data.len()) {
        return Err(format!("Tensor {name}: Form {dims:?} passt nicht zu {} Werten", data.len()));
    }
    Ok((name, Tensor { dims, data }))
}

fn attribute(b: &[u8]) -> Result<(String, Attr), String> {
    let mut l = ProtoReader { b, p: 0 };
    let mut name = String::new();
    let mut a: Option<Attr> = None;
    let (mut ints, mut floats): (Vec<i64>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut type_name = 0u64;
    while !l.at_end() {
        let (f, w) = l.tag()?;
        match (f, w) {
            (1, 2) => name = text(l.bytes()?),
            (2, 5) => a = Some(Attr::Float(f32::from_le_bytes(l.fix32()?))),
            (3, 0) => a = Some(Attr::Int(l.varint()? as i64)),
            (4, 2) => a = Some(Attr::Text(text(l.bytes()?))),
            (5, 2) => a = Some(Attr::Tensor(tensor(l.bytes()?)?.1)),
            (7, 2) => {
                let s = l.bytes()?;
                floats.extend(s.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
            }
            (7, 5) => floats.push(f32::from_le_bytes(l.fix32()?)),
            (8, 2) => {
                let mut s = ProtoReader { b: l.bytes()?, p: 0 };
                while !s.at_end() {
                    ints.push(s.varint()? as i64);
                }
            }
            (8, 0) => ints.push(l.varint()? as i64),
            (20, 0) => type_name = l.varint()?,
            (_, w) => l.skip_field(w)?,
        }
    }
    let a = match type_name {
        7 => Attr::Ints(ints),
        6 => Attr::Floats(floats),
        _ => a.unwrap_or(Attr::Ints(ints)),
    };
    Ok((name, a))
}

fn parse_node(b: &[u8]) -> Result<Node, String> {
    let mut l = ProtoReader { b, p: 0 };
    let mut k = Node::default();
    while !l.at_end() {
        let (f, w) = l.tag()?;
        match (f, w) {
            (1, 2) => k.inputs.push(text(l.bytes()?)),
            (2, 2) => k.outputs.push(text(l.bytes()?)),
            (3, 2) => k.name = text(l.bytes()?),
            (4, 2) => k.op = text(l.bytes()?),
            (5, 2) => {
                let (n, a) = attribute(l.bytes()?)?;
                k.attr.insert(n, a);
            }
            (_, w) => l.skip_field(w)?,
        }
    }
    Ok(k)
}

fn value_name(b: &[u8]) -> String {
    let mut l = ProtoReader { b, p: 0 };
    while !l.at_end() {
        let Ok((f, w)) = l.tag() else { break };
        if f == 1 && w == 2 {
            return l.bytes().map(text).unwrap_or_default();
        }
        if l.skip_field(w).is_err() {
            break;
        }
    }
    String::new()
}

/// Read an ONNX file.
pub fn parse(data: &[u8]) -> Result<Model, String> {
    let mut m = Model::default();
    let mut l = ProtoReader { b: data, p: 0 };
    let mut graph: Option<&[u8]> = None;
    while !l.at_end() {
        let (f, w) = l.tag()?;
        if f == 7 && w == 2 {
            graph = Some(l.bytes()?);
        } else {
            l.skip_field(w)?;
        }
    }
    let mut g = ProtoReader { b: graph.ok_or("ONNX: kein Graph")?, p: 0 };
    while !g.at_end() {
        let (f, w) = g.tag()?;
        match (f, w) {
            (1, 2) => m.nodes.push(parse_node(g.bytes()?)?),
            (5, 2) => {
                let (n, t) = tensor(g.bytes()?)?;
                m.weights.insert(n, t);
            }
            (11, 2) => m.graph_inputs.push(value_name(g.bytes()?)),
            (12, 2) => m.graph_outputs.push(value_name(g.bytes()?)),
            (_, w) => g.skip_field(w)?,
        }
    }
    m.graph_inputs.retain(|n| !m.weights.contains_key(n));
    Ok(m)
}
