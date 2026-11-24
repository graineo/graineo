@group(0) @binding(0) var<storage,read> p:array<f32>;
@group(0) @binding(1) var<storage,read> src:array<f32>;
@group(0) @binding(2) var<storage,read> mask:array<f32>;
@group(0) @binding(3) var<storage,read> fine:array<f32>;
@group(0) @binding(4) var<storage,read> medium:array<f32>;
@group(0) @binding(5) var<storage,read> wide:array<f32>;
@group(0) @binding(6) var<storage,read_write> dst:array<f32>;
fn smooth1(v:f32)->f32 {let t=clamp(v,0.0,1.0);return t*t*(3.0-2.0*t);}
fn lum(v:vec3<f32>)->f32{return dot(v,vec3(0.2126,0.7152,0.0722));}
fn limit1(v:f32,t:f32)->f32{return v/(1.0+(v/t)*(v/t));}
fn enc(v:f32)->f32{let a=abs(v);var e:f32;if a<=0.0031308{e=a*12.92;}else{e=1.055*pow(a,1.0/2.4)-0.055;}return select(e,-e,v<0.0);}
fn dec(e:f32)->f32{let a=abs(e);var v:f32;if a<=0.04045{v=a/12.92;}else{v=pow((a+0.055)/1.055,2.4);}return select(v,-v,e<0.0);}
fn sstep(a:f32,b:f32,x:f32)->f32{return smooth1((x-a)/(b-a));}
// params of version 4: p[16] whites, flag p[15]: p[17] blacks, p[18] dehaze, p[19..28] grading offsets, p[28] grading on,
// p[29] pivot, p[30] curve on, p[31..31+1024] curve table
fn extra(q0:vec3<f32>,y:f32)->vec3<f32>{
 var q=q0;
 // blacks: the global curve (lib.rs blacks_ev), MID_GRAY 0.1845, BLACKS_OFFSET 0.12
 if p[17]!=0.0{let x=log2(max(lum(q),1e-9)/0.1845);let r=0.12*abs(p[17])*exp2(-max(x,-40.0))*(1.0-smooth1((x+2.0)/3.0));
  var ev:f32;if p[17]<0.0{ev=-log2(1.0+r);}else{ev=log2(0.5*(1.0+sqrt(1.0+4.0*r)));} q=q*exp2(ev);}
 if p[18]>0.0{let h=p[18]*0.15;q=(q-vec3(h*0.35))/(1.0-h);}else if p[18]<0.0{let h=-p[18]*0.15;q=q*(1.0-h)+vec3(h*0.35);}
 if p[30]!=0.0{for(var c=0u;c<3u;c++){let e=enc(q[c]);let f=clamp(e,0.0,1.0)*1023.0;let i=min(u32(f),1022u);let t=f-f32(i);let v=p[31u+i]+(p[32u+i]-p[31u+i])*t;q[c]=dec(v+max(e-1.0,0.0));}}
 if p[28]!=0.0{let l=clamp(enc(max(lum(q),0.0)),0.0,1.0);let ws=1.0-sstep(0.0,p[29],l);let wh=sstep(p[29],1.0,l);let wm=max(1.0-ws-wh,0.0);
  for(var c=0u;c<3u;c++){q[c]=dec(enc(q[c])+ws*p[19u+c]+wm*p[22u+c]+wh*p[25u+c]);}}
 return q;
}
@compute @workgroup_size(64) fn box_h(@builtin(global_invocation_id) id:vec3<u32>){
 let w=u32(p[0]);let h=u32(p[1]);if id.x>=h*3u{return;} let y=id.x/3u;let c=id.x%3u;let r=i32(p[2]);var sum=0.0;
 for(var d = -r;d<=r;d++){let x=u32(clamp(d,0,i32(w)-1));sum+=src[(y*w+x)*3u+c];}
 for(var x=0u;x<w;x++){dst[(y*w+x)*3u+c]=sum/f32(2*r+1);let add=min(x+u32(r)+1u,w-1u);let sub=u32(max(i32(x)-r,0));sum+=src[(y*w+add)*3u+c]-src[(y*w+sub)*3u+c];}
}
@compute @workgroup_size(64) fn box_v(@builtin(global_invocation_id) id:vec3<u32>){
 let w=u32(p[0]);let h=u32(p[1]);if id.x>=w*3u{return;} let x=id.x/3u;let c=id.x%3u;let r=i32(p[2]);var sum=0.0;
 for(var d = -r;d<=r;d++){let y=u32(clamp(d,0,i32(h)-1));sum+=src[(y*w+x)*3u+c];}
 for(var y=0u;y<h;y++){dst[(y*w+x)*3u+c]=sum/f32(2*r+1);let add=min(y+u32(r)+1u,h-1u);let sub=u32(max(i32(y)-r,0));sum+=src[(add*w+x)*3u+c]-src[(sub*w+x)*3u+c];}
}
@compute @workgroup_size(16,16) fn correct(@builtin(global_invocation_id) id:vec3<u32>){
 let w=u32(p[0]);let h=u32(p[1]);if id.x>=w||id.y>=h{return;}
 let i=id.y*w+id.x;let j=i*3u;let a=mask[i];let v=vec3(src[j],src[j+1u],src[j+2u]);
 if a==0.0{dst[j]=v.x;dst[j+1u]=v.y;dst[j+2u]=v.z;return;}
 let y=max(lum(v),0.0);var ev=p[4]+p[7]*(1.0-smooth1(y/0.5))+p[6]*smooth1((y-0.25)/0.75);if p[16]!=0.0{ev+=p[16]*smooth1(y-0.5);}
 let gains=exp2(vec3(0.5*p[8]-0.25*p[9],0.5*p[9],-0.5*p[8]-0.25*p[9]));
 var q:vec3<f32>;
 for(var c=0u;c<3u;c++){
  let delta=fine[j+c]-v[c];
  var f=v[c]+p[14]*limit1(delta,0.03)+p[13]*limit1(-delta,0.025)+0.5*p[12]*limit1(v[c]-medium[j+c],0.04)+0.5*p[11]*limit1(v[c]-wide[j+c],0.08);
  f*=exp2(ev)*gains[c];if p[5]!=0.0{f=0.18*pow(abs(f/0.18),1.0+0.5*p[5])*sign(f);}q[c]=f;
 }
 if p[15]!=0.0{q=extra(q,y);}
 let sat=p[10]+0.3*p[18];let yq=lum(q);if sat!=0.0{q=vec3(yq)+(q-vec3(yq))*(1.0+sat);}
 let out=v+a*(q-v);dst[j]=out.x;dst[j+1u]=out.y;dst[j+2u]=out.z;
}
