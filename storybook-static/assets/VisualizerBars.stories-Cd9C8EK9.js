import{j as i}from"./jsx-runtime-Cf8x2fCZ.js";import{r as C}from"./index-Dx_1l3Sb.js";import"./index-yBjzXJbu.js";import"./_commonjsHelpers-CqkleIqs.js";const D="cubic-bezier(0.25, 0.46, 0.45, 0.94)",G="cubic-bezier(0.4, 0, 0.6, 1)",J=120,K=100;function q({levels:e,isRouter:H}){const m=C.useRef(Array(e.length).fill(0));return i.jsx("div",{className:"bars-container",children:e.map((d,u)=>{const p=Math.min(35,7+Math.pow(d,.7)*28),O=m.current[u]??0,g=p>O+.5;m.current[u]=p;const Q=g?D:G,W=g?J:K;return i.jsx("div",{className:`bar${H?" routing-bar":""}`,style:{height:`${p}px`,transition:`height ${W}ms ${Q}, opacity 120ms ease-out`,opacity:Math.max(.2,d*1.7)}},u)})})}q.__docgenInfo={description:"",methods:[],displayName:"VisualizerBars",props:{levels:{required:!0,tsType:{name:"Array",elements:[{name:"number"}],raw:"number[]"},description:""},isRouter:{required:!0,tsType:{name:"boolean"},description:""}}};const Z={title:"Overlay/VisualizerBars",component:q,tags:["autodocs"],parameters:{backgrounds:{default:"overlay",values:[{name:"light",value:"#fbfbfb"},{name:"dark",value:"#2c2b29"},{name:"overlay",value:"#000000cc"}]}},argTypes:{levels:{control:"object",description:"Array of audio level values (0 to 1)"},isRouter:{control:"boolean",description:"Whether the overlay is in router mode (changes bar color)"}},args:{levels:[.3,.5,.7,.5,.3],isRouter:!1},decorators:[e=>i.jsx("div",{style:{background:"#000000cc",borderRadius:"30px",padding:"8px 16px",display:"inline-flex",alignItems:"center"},children:i.jsx(e,{})})]},r={args:{levels:[],isRouter:!1}},s={args:{levels:[.3,.6,.4],isRouter:!1}},a={args:{levels:[.2,.4,.6,.8,.9,.7,.5,.3,.1,.2],isRouter:!1}},o={args:{levels:[1,1,1,1,1,1,1],isRouter:!1}},t={args:{levels:[.05,.1,.08,.05,.12],isRouter:!1}},n={args:{levels:[.3,.5,.7,.5,.3],isRouter:!0}},c={args:{levels:[.2,.4,.6,.8,.9,.7,.5,.3,.1,.2],isRouter:!0}},l={args:{levels:[1,1,1,1,1,1,1],isRouter:!0}};var v,R,f;r.parameters={...r.parameters,docs:{...(v=r.parameters)==null?void 0:v.docs,source:{originalSource:`{
  args: {
    levels: [],
    isRouter: false
  }
}`,...(f=(R=r.parameters)==null?void 0:R.docs)==null?void 0:f.source}}};var y,b,h;s.parameters={...s.parameters,docs:{...(y=s.parameters)==null?void 0:y.docs,source:{originalSource:`{
  args: {
    levels: [0.3, 0.6, 0.4],
    isRouter: false
  }
}`,...(h=(b=s.parameters)==null?void 0:b.docs)==null?void 0:h.source}}};var x,M,S;a.parameters={...a.parameters,docs:{...(x=a.parameters)==null?void 0:x.docs,source:{originalSource:`{
  args: {
    levels: [0.2, 0.4, 0.6, 0.8, 0.9, 0.7, 0.5, 0.3, 0.1, 0.2],
    isRouter: false
  }
}`,...(S=(M=a.parameters)==null?void 0:M.docs)==null?void 0:S.source}}};var B,A,E;o.parameters={...o.parameters,docs:{...(B=o.parameters)==null?void 0:B.docs,source:{originalSource:`{
  args: {
    levels: [1, 1, 1, 1, 1, 1, 1],
    isRouter: false
  }
}`,...(E=(A=o.parameters)==null?void 0:A.docs)==null?void 0:E.source}}};var _,j,L;t.parameters={...t.parameters,docs:{...(_=t.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    levels: [0.05, 0.1, 0.08, 0.05, 0.12],
    isRouter: false
  }
}`,...(L=(j=t.parameters)==null?void 0:j.docs)==null?void 0:L.source}}};var z,w,F;n.parameters={...n.parameters,docs:{...(z=n.parameters)==null?void 0:z.docs,source:{originalSource:`{
  args: {
    levels: [0.3, 0.5, 0.7, 0.5, 0.3],
    isRouter: true
  }
}`,...(F=(w=n.parameters)==null?void 0:w.docs)==null?void 0:F.source}}};var I,$,k;c.parameters={...c.parameters,docs:{...(I=c.parameters)==null?void 0:I.docs,source:{originalSource:`{
  args: {
    levels: [0.2, 0.4, 0.6, 0.8, 0.9, 0.7, 0.5, 0.3, 0.1, 0.2],
    isRouter: true
  }
}`,...(k=($=c.parameters)==null?void 0:$.docs)==null?void 0:k.source}}};var N,T,V;l.parameters={...l.parameters,docs:{...(N=l.parameters)==null?void 0:N.docs,source:{originalSource:`{
  args: {
    levels: [1, 1, 1, 1, 1, 1, 1],
    isRouter: true
  }
}`,...(V=(T=l.parameters)==null?void 0:T.docs)==null?void 0:V.source}}};const ee=["EmptyLevels","FewBars","ManyBars","AllMaxBars","QuietBars","RouterMode","RouterModeManyBars","RouterModeAllMax"];export{o as AllMaxBars,r as EmptyLevels,s as FewBars,a as ManyBars,t as QuietBars,n as RouterMode,l as RouterModeAllMax,c as RouterModeManyBars,ee as __namedExportsOrder,Z as default};
