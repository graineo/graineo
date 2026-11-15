#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# run.sh NAME FILM PAPER 'extra-json' [W H IMAGE]
# Computes the same test image with the Python original of spektrafilm, with spektrafilm-rs (f64, CPU) and with
# spektrafilm-rs (f32, GPU) and compares stage by stage (compare.py).
# Environment (all required except NOPY/PS):
#   WT        working copy of the graineo repo (build.sh built target/ and target-f32/ there)
#   WORKDIR   working folder with test_image.npy/.f64 (test_image.py) and the Python original in src/spektrafilm/src
#   PY        Python with numpy, colour-science and the dependencies of spektrafilm (e.g. venv/bin/python)
#   NOPY=1    skip the Python run (use existing py_NAME files);  PS=tile size (16)
: "${WT:?set WT}"; : "${WORKDIR:?set WORKDIR}"; : "${PY:?set PY}"
T="$(cd "$(dirname "$0")" && pwd)"
D=$WT/third_party/spektrafilm-rs/data
N=$1; F=$2; P=$3; X=${4:-{\}}; W=${5:-384}; H=${6:-80}; IMG=${7:-$WORKDIR/test_image}
cd "$WORKDIR"
$PY -I - "$X" <<PYEND > $WORKDIR/$N.rs.json
import json,sys
base={"io":{"input_color_space":"sRGB"},"camera":{"auto_exposure":False},"film_render":{"grain":{"active":False},"glare":{"active":False}},"print_render":{"glare":{"active":False}}}
def m(a,b):
  for k,v in b.items():
    if isinstance(v,dict): m(a.setdefault(k,{}),v)
    else: a[k]=v
m(base,json.loads(sys.argv[1])); print(json.dumps(base))
PYEND
python3 -c "import json;print(json.dumps({'input':'$IMG.npy','film':'$F','print':'$P','params':json.load(open('$WORKDIR/$N.rs.json'))}))" > $WORKDIR/$N.py.json
[ -n "$NOPY" ] || $PY -I $T/py_run.py $WORKDIR/$N.py.json $WORKDIR/py_$N $WORKDIR/src/spektrafilm/src >/dev/null 2>&1 || echo PYERROR
$WT/target/release/examples/paritaet $D $F $P $WORKDIR/$N.rs.json $IMG.f64 $W $H $WORKDIR/rs_$N cpu 2>/dev/null
$WT/target-f32/release/examples/paritaet $D $F $P $WORKDIR/$N.rs.json $IMG.f64 $W $H $WORKDIR/gpu_$N gpu 2>/dev/null; rm -f $WORKDIR/gpu_$N.log_e* $WORKDIR/gpu_$N.cmy*
echo "== $N: Python vs Rust CPU f64"; $PY -I $T/compare.py py_$N rs_$N $W $H ${PS:-16}
echo "== $N: Python vs Rust GPU f32"; $PY -I $T/compare.py py_$N gpu_$N $W $H ${PS:-16} | grep -A2 rgb_out
