import{j as e}from"./jsx-runtime-Cf8x2fCZ.js";import{S as B}from"./SettingContainer-B0lNAYJp.js";import"./index-yBjzXJbu.js";import"./index-Dx_1l3Sb.js";import"./_commonjsHelpers-CqkleIqs.js";import"./index-DML4njjH.js";import"./index-BLHw34Di.js";const E=({checked:G,onChange:H,disabled:l=!1,isUpdating:d=!1,label:P,description:O,descriptionMode:R="tooltip",grouped:$=!1,tooltipPosition:z="top"})=>e.jsxs(B,{title:P,description:O,descriptionMode:R,grouped:$,disabled:l,tooltipPosition:z,children:[e.jsxs("label",{className:`inline-flex items-center ${l||d?"cursor-not-allowed":"cursor-pointer"}`,children:[e.jsx("input",{type:"checkbox",value:"",className:"sr-only peer",checked:G,disabled:l||d,onChange:A=>H(A.target.checked)}),e.jsx("div",{className:"relative w-11 h-6 bg-mid-gray/20 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-logo-primary rounded-full peer peer-checked:after:translate-x-full rtl:peer-checked:after:-translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-background-ui peer-disabled:opacity-50"})]}),d&&e.jsx("div",{className:"absolute inset-0 flex items-center justify-center",children:e.jsx("div",{className:"w-4 h-4 border-2 border-logo-primary border-t-transparent rounded-full animate-spin"})})]});E.__docgenInfo={description:"",methods:[],displayName:"ToggleSwitch",props:{checked:{required:!0,tsType:{name:"boolean"},description:""},onChange:{required:!0,tsType:{name:"signature",type:"function",raw:"(checked: boolean) => void",signature:{arguments:[{type:{name:"boolean"},name:"checked"}],return:{name:"void"}}},description:""},disabled:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"false",computed:!1}},isUpdating:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"false",computed:!1}},label:{required:!0,tsType:{name:"string"},description:""},description:{required:!0,tsType:{name:"string"},description:""},descriptionMode:{required:!1,tsType:{name:"union",raw:'"inline" | "tooltip"',elements:[{name:"literal",value:'"inline"'},{name:"literal",value:'"tooltip"'}]},description:"",defaultValue:{value:'"tooltip"',computed:!1}},grouped:{required:!1,tsType:{name:"boolean"},description:"",defaultValue:{value:"false",computed:!1}},tooltipPosition:{required:!1,tsType:{name:"union",raw:'"top" | "bottom"',elements:[{name:"literal",value:'"top"'},{name:"literal",value:'"bottom"'}]},description:"",defaultValue:{value:'"top"',computed:!1}}}};const Y={title:"UI/ToggleSwitch",component:E,tags:["autodocs"],argTypes:{checked:{control:"boolean"},disabled:{control:"boolean"},isUpdating:{control:"boolean"},label:{control:"text"},description:{control:"text"},descriptionMode:{control:"select",options:["inline","tooltip"]},grouped:{control:"boolean"},tooltipPosition:{control:"select",options:["top","bottom"]},onChange:{action:"changed"}},args:{checked:!1,label:"Enable feature",description:"Turn this feature on or off",descriptionMode:"tooltip"}},r={args:{checked:!1}},o={args:{checked:!0}},t={args:{checked:!1,disabled:!0}},a={args:{checked:!0,disabled:!0}},s={args:{descriptionMode:"inline",description:"This description appears inline below the label"}},n={args:{descriptionMode:"tooltip",description:"Hover the info icon to see this description"}},i={args:{isUpdating:!0}},c={args:{grouped:!0,description:"This toggle is part of a grouped setting"}};var p,u,m;r.parameters={...r.parameters,docs:{...(p=r.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    checked: false
  }
}`,...(m=(u=r.parameters)==null?void 0:u.docs)==null?void 0:m.source}}};var g,f,h;o.parameters={...o.parameters,docs:{...(g=o.parameters)==null?void 0:g.docs,source:{originalSource:`{
  args: {
    checked: true
  }
}`,...(h=(f=o.parameters)==null?void 0:f.docs)==null?void 0:h.source}}};var b,k,y;t.parameters={...t.parameters,docs:{...(b=t.parameters)==null?void 0:b.docs,source:{originalSource:`{
  args: {
    checked: false,
    disabled: true
  }
}`,...(y=(k=t.parameters)==null?void 0:k.docs)==null?void 0:y.source}}};var T,x,v;a.parameters={...a.parameters,docs:{...(T=a.parameters)==null?void 0:T.docs,source:{originalSource:`{
  args: {
    checked: true,
    disabled: true
  }
}`,...(v=(x=a.parameters)==null?void 0:x.docs)==null?void 0:v.source}}};var w,S,j;s.parameters={...s.parameters,docs:{...(w=s.parameters)==null?void 0:w.docs,source:{originalSource:`{
  args: {
    descriptionMode: "inline",
    description: "This description appears inline below the label"
  }
}`,...(j=(S=s.parameters)==null?void 0:S.docs)==null?void 0:j.source}}};var q,C,D;n.parameters={...n.parameters,docs:{...(q=n.parameters)==null?void 0:q.docs,source:{originalSource:`{
  args: {
    descriptionMode: "tooltip",
    description: "Hover the info icon to see this description"
  }
}`,...(D=(C=n.parameters)==null?void 0:C.docs)==null?void 0:D.source}}};var M,U,N;i.parameters={...i.parameters,docs:{...(M=i.parameters)==null?void 0:M.docs,source:{originalSource:`{
  args: {
    isUpdating: true
  }
}`,...(N=(U=i.parameters)==null?void 0:U.docs)==null?void 0:N.source}}};var V,I,_;c.parameters={...c.parameters,docs:{...(V=c.parameters)==null?void 0:V.docs,source:{originalSource:`{
  args: {
    grouped: true,
    description: "This toggle is part of a grouped setting"
  }
}`,...(_=(I=c.parameters)==null?void 0:I.docs)==null?void 0:_.source}}};const Z=["Unchecked","Checked","Disabled","DisabledChecked","InlineDescription","TooltipDescription","Updating","Grouped"];export{o as Checked,t as Disabled,a as DisabledChecked,c as Grouped,s as InlineDescription,n as TooltipDescription,r as Unchecked,i as Updating,Z as __namedExportsOrder,Y as default};
