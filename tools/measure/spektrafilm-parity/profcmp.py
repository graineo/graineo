# SPDX-License-Identifier: GPL-3.0-only
# profcmp.py FOLDER_A FOLDER_B : compares same-named film profile JSON files of two folders field by field,
# output per file: largest deviation (0 = equal). Purpose: check profiles against the reference after a rebuild/update.
import json,sys,os,math
P,R=sys.argv[1],sys.argv[2]
def flat(o,p=''):
    if isinstance(o,dict):
        for k,v in o.items(): yield from flat(v,p+'/'+k)
    elif isinstance(o,list):
        if o and all(isinstance(x,(int,float)) or x is None for x in o):
            yield p,o
        else:
            for i,v in enumerate(o): yield from flat(v,p+f'[{i}]')
    else: yield p,o
def num(x):
    return x if isinstance(x,(int,float)) and x is not None else float('nan')
for f in sorted(os.listdir(P)):
    a=json.load(open(os.path.join(P,f))); b=json.load(open(os.path.join(R,f)))
    fa=dict(flat(a)); fb=dict(flat(b))
    diffs=[]
    for k in sorted(set(fa)|set(fb)):
        if '/card' in k: continue
        if k not in fa: diffs.append(('onlyRust',k)); continue
        if k not in fb: diffs.append(('onlyPy',k,str(fa[k])[:60])); continue
        va,vb=fa[k],fb[k]
        if isinstance(va,list) and isinstance(vb,list):
            if len(va)!=len(vb): diffs.append(('len',k,len(va),len(vb))); continue
            m=0
            for x,y in zip(va,vb):
                if x is None or y is None:
                    if (x is None)!=(y is None): m=float('inf')
                    continue
                if isinstance(x,float) and math.isnan(x) and isinstance(y,float) and math.isnan(y): continue
                m=max(m,abs(x-y))
            if m>1e-9: diffs.append(('val',k,m))
        elif va!=vb: diffs.append(('ne',k,str(va)[:50],str(vb)[:50]))
    print(f, len(diffs))
    for d in diffs[:12]: print('   ',d)
