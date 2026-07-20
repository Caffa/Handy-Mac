import{j as Z}from"./jsx-runtime-Cf8x2fCZ.js";import"./index-yBjzXJbu.js";const A=({children:F,className:H="",variant:J="primary",size:K="md",...Q})=>{const W="font-medium rounded-lg border focus:outline-none transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer",X={primary:"text-white bg-background-ui border-background-ui hover:bg-background-ui/80 hover:border-background-ui/80 focus:ring-1 focus:ring-background-ui","primary-soft":"text-text bg-logo-primary/20 border-transparent hover:bg-logo-primary/30 focus:ring-1 focus:ring-logo-primary",secondary:"bg-mid-gray/10 border-mid-gray/20 hover:bg-background-ui/30 hover:border-logo-primary focus:outline-none",danger:"text-white bg-red-600 border-mid-gray/20 hover:bg-red-700 hover:border-red-700 focus:ring-1 focus:ring-red-500","danger-ghost":"text-red-400 border-transparent hover:text-red-300 hover:bg-red-500/10 focus:bg-red-500/20",ghost:"text-current border-transparent hover:bg-mid-gray/10 hover:border-logo-primary focus:bg-mid-gray/20"},Y={sm:"px-2 py-1 text-xs",md:"px-4 py-[5px] text-sm",lg:"px-4 py-2 text-base"};return Z.jsx("button",{className:`${W} ${X[J]} ${Y[K]} ${H}`,...Q,children:F})};A.__docgenInfo={description:"",methods:[],displayName:"Button",props:{variant:{required:!1,tsType:{name:"union",raw:`| "primary"
| "primary-soft"
| "secondary"
| "danger"
| "danger-ghost"
| "ghost"`,elements:[{name:"literal",value:'"primary"'},{name:"literal",value:'"primary-soft"'},{name:"literal",value:'"secondary"'},{name:"literal",value:'"danger"'},{name:"literal",value:'"danger-ghost"'},{name:"literal",value:'"ghost"'}]},description:"",defaultValue:{value:'"primary"',computed:!1}},size:{required:!1,tsType:{name:"union",raw:'"sm" | "md" | "lg"',elements:[{name:"literal",value:'"sm"'},{name:"literal",value:'"md"'},{name:"literal",value:'"lg"'}]},description:"",defaultValue:{value:'"md"',computed:!1}},className:{defaultValue:{value:'""',computed:!1},required:!1}}};const ar={title:"UI/Button",component:A,tags:["autodocs"],argTypes:{variant:{control:"select",options:["primary","primary-soft","secondary","danger","danger-ghost","ghost"]},size:{control:"select",options:["sm","md","lg"]},disabled:{control:"boolean"},onClick:{action:"clicked"}},args:{children:"Click me",variant:"primary",size:"md"}},r={args:{variant:"primary"}},e={args:{variant:"primary-soft"}},a={args:{variant:"secondary"}},s={args:{variant:"danger"}},o={args:{variant:"danger-ghost"}},t={args:{variant:"ghost"}},n={args:{size:"sm"}},i={args:{size:"md"}},d={args:{size:"lg"}},c={args:{disabled:!0}},m={args:{variant:"danger",disabled:!0}};var g,l,u;r.parameters={...r.parameters,docs:{...(g=r.parameters)==null?void 0:g.docs,source:{originalSource:`{
  args: {
    variant: "primary"
  }
}`,...(u=(l=r.parameters)==null?void 0:l.docs)==null?void 0:u.source}}};var p,y,b;e.parameters={...e.parameters,docs:{...(p=e.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    variant: "primary-soft"
  }
}`,...(b=(y=e.parameters)==null?void 0:y.docs)==null?void 0:b.source}}};var v,f,h;a.parameters={...a.parameters,docs:{...(v=a.parameters)==null?void 0:v.docs,source:{originalSource:`{
  args: {
    variant: "secondary"
  }
}`,...(h=(f=a.parameters)==null?void 0:f.docs)==null?void 0:h.source}}};var x,S,z;s.parameters={...s.parameters,docs:{...(x=s.parameters)==null?void 0:x.docs,source:{originalSource:`{
  args: {
    variant: "danger"
  }
}`,...(z=(S=s.parameters)==null?void 0:S.docs)==null?void 0:z.source}}};var D,k,w;o.parameters={...o.parameters,docs:{...(D=o.parameters)==null?void 0:D.docs,source:{originalSource:`{
  args: {
    variant: "danger-ghost"
  }
}`,...(w=(k=o.parameters)==null?void 0:k.docs)==null?void 0:w.source}}};var C,G,P;t.parameters={...t.parameters,docs:{...(C=t.parameters)==null?void 0:C.docs,source:{originalSource:`{
  args: {
    variant: "ghost"
  }
}`,...(P=(G=t.parameters)==null?void 0:G.docs)==null?void 0:P.source}}};var _,$,j;n.parameters={...n.parameters,docs:{...(_=n.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    size: "sm"
  }
}`,...(j=($=n.parameters)==null?void 0:$.docs)==null?void 0:j.source}}};var q,B,N;i.parameters={...i.parameters,docs:{...(q=i.parameters)==null?void 0:q.docs,source:{originalSource:`{
  args: {
    size: "md"
  }
}`,...(N=(B=i.parameters)==null?void 0:B.docs)==null?void 0:N.source}}};var T,V,E;d.parameters={...d.parameters,docs:{...(T=d.parameters)==null?void 0:T.docs,source:{originalSource:`{
  args: {
    size: "lg"
  }
}`,...(E=(V=d.parameters)==null?void 0:V.docs)==null?void 0:E.source}}};var I,L,M;c.parameters={...c.parameters,docs:{...(I=c.parameters)==null?void 0:I.docs,source:{originalSource:`{
  args: {
    disabled: true
  }
}`,...(M=(L=c.parameters)==null?void 0:L.docs)==null?void 0:M.source}}};var O,R,U;m.parameters={...m.parameters,docs:{...(O=m.parameters)==null?void 0:O.docs,source:{originalSource:`{
  args: {
    variant: "danger",
    disabled: true
  }
}`,...(U=(R=m.parameters)==null?void 0:R.docs)==null?void 0:U.source}}};const sr=["Primary","PrimarySoft","Secondary","Danger","DangerGhost","Ghost","Small","Medium","Large","Disabled","DisabledDanger"];export{s as Danger,o as DangerGhost,c as Disabled,m as DisabledDanger,t as Ghost,d as Large,i as Medium,r as Primary,e as PrimarySoft,a as Secondary,n as Small,sr as __namedExportsOrder,ar as default};
