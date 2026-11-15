# SPDX-License-Identifier: GPL-3.0-only
"""Sigma analog: noise table of the layers per ISO.

Call per photo (own test copy, own GRAINEO_DATA_DIR):
  graineo-dev analog szene PHOTO.X3F t.f32 --layers identity > t.json
  python -I noise.py t.f32 t.json >> noise.jsonl
Then weights.py noise.jsonl … (median per ISO).
"""
import json, sys, numpy as np
from scipy.ndimage import gaussian_filter
info=json.load(open(sys.argv[2])); W,H=info['width'],info['height']
L=np.fromfile(sys.argv[1],dtype='<f4').reshape(H,W,3).astype(np.float64)
lo=np.stack([gaussian_filter(L[...,c],4) for c in range(3)],-1); R=L-lo
B=32; hb,wb=H//B,W//B
blk=lambda x: x[:hb*B,:wb*B].reshape(hb,B,wb,B,3).transpose(0,2,1,3,4).reshape(-1,B*B,3)
X=blk(R); M=blk(lo).mean(1); struct=blk(lo).std(1).mean(1)
C=np.einsum('nic,nid->ncd',X,X)/(B*B)
S=M.mean(1); ok=(M.min(1)>0.04)&(M.max(1)<0.6)
rel=C/(M[:,:,None]*M[:,None,:])*(S/0.184)[:,None,None]
idx=np.where(ok)[0]; sel=idx[(struct/S)[idx]<=np.quantile((struct/S)[idx],0.2)]
c=np.median(rel[sel],0); sd=np.sqrt(np.diag(c))
print(json.dumps({'iso':info['iso'],'camera':info['camera'],'blocks':int(len(sel)),'cov':c.tolist(),'sd':sd.tolist(),'corr':(c/np.outer(sd,sd)).round(3).tolist()}))
