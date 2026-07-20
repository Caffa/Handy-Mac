import{j as F}from"./jsx-runtime-Cf8x2fCZ.js";import"./index-yBjzXJbu.js";const O=({className:R="",variant:U="default",disabled:p,...k})=>{const z="px-2 py-1 text-sm font-semibold bg-mid-gray/10 border border-mid-gray/80 rounded-md text-start transition-all duration-150",A=p?"opacity-60 cursor-not-allowed bg-mid-gray/10 border-mid-gray/40":"hover:bg-logo-primary/10 hover:border-logo-primary focus:outline-none focus:bg-logo-primary/20 focus:border-logo-primary",B={default:"px-3 py-2",compact:"px-2 py-1"};return F.jsx("input",{className:`${z} ${B[U]} ${A} ${R}`,disabled:p,...k})};O.__docgenInfo={description:"",methods:[],displayName:"Input",props:{variant:{required:!1,tsType:{name:"union",raw:'"default" | "compact"',elements:[{name:"literal",value:'"default"'},{name:"literal",value:'"compact"'}]},description:"",defaultValue:{value:'"default"',computed:!1}},className:{defaultValue:{value:'""',computed:!1},required:!1}}};const K={title:"UI/Input",component:O,tags:["autodocs"],argTypes:{variant:{control:"select",options:["default","compact"]},disabled:{control:"boolean"},placeholder:{control:"text"},value:{control:"text"},type:{control:"select",options:["text","password","email","number","search","url"]}},args:{placeholder:"Enter text...",variant:"default"}},e={args:{variant:"default",placeholder:"Default input"}},a={args:{variant:"compact",placeholder:"Compact input"}},r={args:{value:"Hello, world",placeholder:"Enter text..."}},t={args:{disabled:!0,placeholder:"Disabled input"}},s={args:{disabled:!0,value:"Cannot edit this"}},o={args:{type:"password",placeholder:"Enter password..."}},n={args:{type:"email",placeholder:"user@example.com"}},l={args:{type:"number",placeholder:"0",min:0,max:100}},c={args:{type:"search",placeholder:"Search...",variant:"default"}};var d,u,i;e.parameters={...e.parameters,docs:{...(d=e.parameters)==null?void 0:d.docs,source:{originalSource:`{
  args: {
    variant: "default",
    placeholder: "Default input"
  }
}`,...(i=(u=e.parameters)==null?void 0:u.docs)==null?void 0:i.source}}};var m,g,h;a.parameters={...a.parameters,docs:{...(m=a.parameters)==null?void 0:m.docs,source:{originalSource:`{
  args: {
    variant: "compact",
    placeholder: "Compact input"
  }
}`,...(h=(g=a.parameters)==null?void 0:g.docs)==null?void 0:h.source}}};var f,b,y;r.parameters={...r.parameters,docs:{...(f=r.parameters)==null?void 0:f.docs,source:{originalSource:`{
  args: {
    value: "Hello, world",
    placeholder: "Enter text..."
  }
}`,...(y=(b=r.parameters)==null?void 0:b.docs)==null?void 0:y.source}}};var v,x,I;t.parameters={...t.parameters,docs:{...(v=t.parameters)==null?void 0:v.docs,source:{originalSource:`{
  args: {
    disabled: true,
    placeholder: "Disabled input"
  }
}`,...(I=(x=t.parameters)==null?void 0:x.docs)==null?void 0:I.source}}};var S,w,D;s.parameters={...s.parameters,docs:{...(S=s.parameters)==null?void 0:S.docs,source:{originalSource:`{
  args: {
    disabled: true,
    value: "Cannot edit this"
  }
}`,...(D=(w=s.parameters)==null?void 0:w.docs)==null?void 0:D.source}}};var C,E,V;o.parameters={...o.parameters,docs:{...(C=o.parameters)==null?void 0:C.docs,source:{originalSource:`{
  args: {
    type: "password",
    placeholder: "Enter password..."
  }
}`,...(V=(E=o.parameters)==null?void 0:E.docs)==null?void 0:V.source}}};var N,W,_;n.parameters={...n.parameters,docs:{...(N=n.parameters)==null?void 0:N.docs,source:{originalSource:`{
  args: {
    type: "email",
    placeholder: "user@example.com"
  }
}`,...(_=(W=n.parameters)==null?void 0:W.docs)==null?void 0:_.source}}};var $,j,q;l.parameters={...l.parameters,docs:{...($=l.parameters)==null?void 0:$.docs,source:{originalSource:`{
  args: {
    type: "number",
    placeholder: "0",
    min: 0,
    max: 100
  }
}`,...(q=(j=l.parameters)==null?void 0:j.docs)==null?void 0:q.source}}};var H,P,T;c.parameters={...c.parameters,docs:{...(H=c.parameters)==null?void 0:H.docs,source:{originalSource:`{
  args: {
    type: "search",
    placeholder: "Search...",
    variant: "default"
  }
}`,...(T=(P=c.parameters)==null?void 0:P.docs)==null?void 0:T.source}}};const L=["Default","Compact","WithValue","Disabled","DisabledWithValue","PasswordInput","EmailInput","NumberInput","SearchInput"];export{a as Compact,e as Default,t as Disabled,s as DisabledWithValue,n as EmailInput,l as NumberInput,o as PasswordInput,c as SearchInput,r as WithValue,L as __namedExportsOrder,K as default};
