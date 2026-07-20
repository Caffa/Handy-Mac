import{j as a}from"./jsx-runtime-Cf8x2fCZ.js";import{S as Y}from"./SettingContainer-B0lNAYJp.js";import"./index-yBjzXJbu.js";import"./index-Dx_1l3Sb.js";import"./_commonjsHelpers-CqkleIqs.js";import"./index-DML4njjH.js";import"./index-BLHw34Di.js";const P=({value:e,onChange:Q,min:r,max:d,step:z=.01,disabled:g=!1,label:U,description:B,descriptionMode:J="tooltip",grouped:K=!1,showValue:L=!0,formatValue:W=m=>m.toFixed(2)})=>{const m=X=>{Q(parseFloat(X.target.value))};return a.jsx(Y,{title:U,description:B,descriptionMode:J,grouped:K,layout:"horizontal",disabled:g,children:a.jsx("div",{className:"w-full",children:a.jsxs("div",{className:"flex items-center space-x-1 h-6",children:[a.jsx("input",{type:"range",min:r,max:d,step:z,value:e,onChange:m,disabled:g,className:"flex-grow h-2 rounded-lg appearance-none cursor-pointer focus:outline-none focus:ring-2 focus:ring-logo-primary disabled:opacity-50 disabled:cursor-not-allowed",style:{background:`linear-gradient(to right, var(--color-background-ui) ${(e-r)/(d-r)*100}%, rgba(128, 128, 128, 0.2) ${(e-r)/(d-r)*100}%)`}}),L&&a.jsx("span",{className:"text-sm font-medium text-text/90 w-12 text-end",children:W(e)})]})})})};P.__docgenInfo={description:"",methods:[],displayName:"Slider",props:{value:{required:!0,tsType:{name:"number"},description:""},onChange:{required:!0,tsType:{name:"signature",type:"function",raw:"(value: number) => void",signature:{arguments:[{type:{name:"number"},name:"value"}],return:{name:"void"}}},description:""},min:{required:!0,tsType:{name:"number"},description:""},max:{required:!0,tsType:{name:"number"},description:""},step:{required:!1,tsType:{name:"number"},description:"",defaultValue:{value:"0.01",computed:!1}},disabled:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"false",computed:!1}},label:{required:!0,tsType:{name:"string"},description:""},description:{required:!0,tsType:{name:"string"},description:""},descriptionMode:{required:!1,tsType:{name:"union",raw:'"inline" | "tooltip"',elements:[{name:"literal",value:'"inline"'},{name:"literal",value:'"tooltip"'}]},description:"",defaultValue:{value:'"tooltip"',computed:!1}},grouped:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"false",computed:!1}},showValue:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"true",computed:!1}},formatValue:{required:!1,tsType:{name:"signature",type:"function",raw:"(value: number) => string",signature:{arguments:[{type:{name:"number"},name:"value"}],return:{name:"string"}}},description:"",defaultValue:{value:"(v) => v.toFixed(2)",computed:!1}}}};const se={title:"UI/Slider",component:P,tags:["autodocs"],argTypes:{value:{control:"number"},min:{control:"number"},max:{control:"number"},step:{control:"number"},disabled:{control:"boolean"},label:{control:"text"},description:{control:"text"},descriptionMode:{control:"select",options:["inline","tooltip"]},grouped:{control:"boolean"},showValue:{control:"boolean"},onChange:{action:"changed"}},args:{value:.5,min:0,max:1,step:.01,label:"Volume",description:"Adjust the output volume level",showValue:!0}},t={args:{value:.5}},n={args:{value:0}},o={args:{value:1}},s={args:{value:75,min:0,max:100,step:1,formatValue:e=>`${Math.round(e)}%`,label:"Opacity",description:"Set the overlay opacity percentage"}},i={args:{value:5,min:0,max:10,step:1,formatValue:e=>`${Math.round(e)}/10`,label:"Quality",description:"Set the quality level from 1 to 10"}},l={args:{value:.5,disabled:!0}},u={args:{value:.75,showValue:!1}},c={args:{descriptionMode:"inline",description:"This description appears inline below the label"}},p={args:{grouped:!0}};var f,v,b;t.parameters={...t.parameters,docs:{...(f=t.parameters)==null?void 0:f.docs,source:{originalSource:`{
  args: {
    value: 0.5
  }
}`,...(b=(v=t.parameters)==null?void 0:v.docs)==null?void 0:b.source}}};var h,y,x;n.parameters={...n.parameters,docs:{...(h=n.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    value: 0
  }
}`,...(x=(y=n.parameters)==null?void 0:y.docs)==null?void 0:x.source}}};var V,S,T;o.parameters={...o.parameters,docs:{...(V=o.parameters)==null?void 0:V.docs,source:{originalSource:`{
  args: {
    value: 1
  }
}`,...(T=(S=o.parameters)==null?void 0:S.docs)==null?void 0:T.source}}};var q,w,M;s.parameters={...s.parameters,docs:{...(q=s.parameters)==null?void 0:q.docs,source:{originalSource:`{
  args: {
    value: 75,
    min: 0,
    max: 100,
    step: 1,
    formatValue: (v: number) => \`\${Math.round(v)}%\`,
    label: "Opacity",
    description: "Set the overlay opacity percentage"
  }
}`,...(M=(w=s.parameters)==null?void 0:w.docs)==null?void 0:M.source}}};var j,D,I;i.parameters={...i.parameters,docs:{...(j=i.parameters)==null?void 0:j.docs,source:{originalSource:`{
  args: {
    value: 5,
    min: 0,
    max: 10,
    step: 1,
    formatValue: (v: number) => \`\${Math.round(v)}/10\`,
    label: "Quality",
    description: "Set the quality level from 1 to 10"
  }
}`,...(I=(D=i.parameters)==null?void 0:D.docs)==null?void 0:I.source}}};var $,A,C;l.parameters={...l.parameters,docs:{...($=l.parameters)==null?void 0:$.docs,source:{originalSource:`{
  args: {
    value: 0.5,
    disabled: true
  }
}`,...(C=(A=l.parameters)==null?void 0:A.docs)==null?void 0:C.source}}};var N,R,_;u.parameters={...u.parameters,docs:{...(N=u.parameters)==null?void 0:N.docs,source:{originalSource:`{
  args: {
    value: 0.75,
    showValue: false
  }
}`,...(_=(R=u.parameters)==null?void 0:R.docs)==null?void 0:_.source}}};var F,O,k;c.parameters={...c.parameters,docs:{...(F=c.parameters)==null?void 0:F.docs,source:{originalSource:`{
  args: {
    descriptionMode: "inline",
    description: "This description appears inline below the label"
  }
}`,...(k=(O=c.parameters)==null?void 0:O.docs)==null?void 0:k.source}}};var E,G,H;p.parameters={...p.parameters,docs:{...(E=p.parameters)==null?void 0:E.docs,source:{originalSource:`{
  args: {
    grouped: true
  }
}`,...(H=(G=p.parameters)==null?void 0:G.docs)==null?void 0:H.source}}};const ie=["Default","AtMinimum","AtMaximum","PercentageRange","IntegerRange","Disabled","HiddenValue","InlineDescription","Grouped"];export{o as AtMaximum,n as AtMinimum,t as Default,l as Disabled,p as Grouped,u as HiddenValue,c as InlineDescription,i as IntegerRange,s as PercentageRange,ie as __namedExportsOrder,se as default};
