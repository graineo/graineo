use std::collections::BTreeMap;
fn main() {
    let p = std::env::args().nth(1).expect("ONNX file");
    let m = faces::onnx::parse(&std::fs::read(&p).unwrap()).unwrap();
    let mut ops: BTreeMap<&str, usize> = BTreeMap::new();
    for k in &m.nodes {
        *ops.entry(&k.op).or_default() += 1;
    }
    println!("inputs {:?}, outputs {:?}, nodes {}, weights {}", m.graph_inputs, m.graph_outputs, m.nodes.len(), m.weights.len());
    println!("{ops:?}");
    for k in m.nodes.iter().take(14) {
        let attrs: Vec<String> = k.attr.iter().map(|(n, a)| format!("{n}={a:?}")).collect();
        println!("{} {} in={:?} out={:?} {}", k.op, k.name, k.inputs, k.outputs, attrs.join(" "));
    }
    for k in m.nodes.iter().rev().take(6) {
        println!("… {} {} in={:?} out={:?}", k.op, k.name, k.inputs, k.outputs);
    }
}
