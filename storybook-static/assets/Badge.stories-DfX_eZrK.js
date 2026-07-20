import{j as W}from"./jsx-runtime-Cf8x2fCZ.js";import"./index-yBjzXJbu.js";const k=({children:A,variant:D="primary",className:E="",title:P})=>{const V={primary:"bg-logo-primary",success:"bg-green-500/20 text-green-400",secondary:"bg-mid-gray/20 text-text/70"};return W.jsx("span",{className:`inline-flex items-center px-3 py-1 rounded-full text-xs font-medium ${V[D]} ${E}`,title:P,children:A})};k.__docgenInfo={description:"",methods:[],displayName:"Badge",props:{children:{required:!0,tsType:{name:"ReactReactNode",raw:"React.ReactNode"},description:""},variant:{required:!1,tsType:{name:"union",raw:'"primary" | "success" | "secondary"',elements:[{name:"literal",value:'"primary"'},{name:"literal",value:'"success"'},{name:"literal",value:'"secondary"'}]},description:"",defaultValue:{value:'"primary"',computed:!1}},className:{required:!1,tsType:{name:"string"},description:"",defaultValue:{value:'""',computed:!1}},title:{required:!1,tsType:{name:"string"},description:""}}};const U={title:"UI/Badge",component:k,tags:["autodocs"],argTypes:{variant:{control:"select",options:["primary","success","secondary"]},className:{control:"text"},title:{control:"text"}},args:{children:"Badge text",variant:"primary"}},e={args:{variant:"primary"}},r={args:{variant:"success"}},a={args:{variant:"secondary"}},s={args:{variant:"primary",title:"This is a tooltip title"}},t={args:{variant:"success",className:"uppercase tracking-wider"}},n={args:{children:"42",variant:"primary"}},c={args:{children:"Active",variant:"success"}},o={args:{children:"Draft",variant:"secondary"}};var i,m,d;e.parameters={...e.parameters,docs:{...(i=e.parameters)==null?void 0:i.docs,source:{originalSource:`{
  args: {
    variant: "primary"
  }
}`,...(d=(m=e.parameters)==null?void 0:m.docs)==null?void 0:d.source}}};var p,u,l;r.parameters={...r.parameters,docs:{...(p=r.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    variant: "success"
  }
}`,...(l=(u=r.parameters)==null?void 0:u.docs)==null?void 0:l.source}}};var g,y,v;a.parameters={...a.parameters,docs:{...(g=a.parameters)==null?void 0:g.docs,source:{originalSource:`{
  args: {
    variant: "secondary"
  }
}`,...(v=(y=a.parameters)==null?void 0:y.docs)==null?void 0:v.source}}};var f,x,S;s.parameters={...s.parameters,docs:{...(f=s.parameters)==null?void 0:f.docs,source:{originalSource:`{
  args: {
    variant: "primary",
    title: "This is a tooltip title"
  }
}`,...(S=(x=s.parameters)==null?void 0:x.docs)==null?void 0:S.source}}};var h,N,B;t.parameters={...t.parameters,docs:{...(h=t.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    variant: "success",
    className: "uppercase tracking-wider"
  }
}`,...(B=(N=t.parameters)==null?void 0:N.docs)==null?void 0:B.source}}};var T,b,C;n.parameters={...n.parameters,docs:{...(T=n.parameters)==null?void 0:T.docs,source:{originalSource:`{
  args: {
    children: "42",
    variant: "primary"
  }
}`,...(C=(b=n.parameters)==null?void 0:b.docs)==null?void 0:C.source}}};var R,q,w;c.parameters={...c.parameters,docs:{...(R=c.parameters)==null?void 0:R.docs,source:{originalSource:`{
  args: {
    children: "Active",
    variant: "success"
  }
}`,...(w=(q=c.parameters)==null?void 0:q.docs)==null?void 0:w.source}}};var I,_,j;o.parameters={...o.parameters,docs:{...(I=o.parameters)==null?void 0:I.docs,source:{originalSource:`{
  args: {
    children: "Draft",
    variant: "secondary"
  }
}`,...(j=(_=o.parameters)==null?void 0:_.docs)==null?void 0:j.source}}};const z=["Primary","Success","Secondary","WithTitle","CustomClassName","NumberBadge","StatusBadge","InfoBadge"];export{t as CustomClassName,o as InfoBadge,n as NumberBadge,e as Primary,a as Secondary,c as StatusBadge,r as Success,s as WithTitle,z as __namedExportsOrder,U as default};
