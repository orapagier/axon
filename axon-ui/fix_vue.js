import fs from 'fs';
let content = fs.readFileSync('src/components/NodeDetails.vue', 'utf8');

content = content.replace(/\.fc-item-fields \{.*?display: flex; gap: 8px; flex: 1;.*?\}/s,
    '.fc-item-fields { display: flex; flex-wrap: wrap; gap: 8px; flex: 1; }');

content = content.replace(/\.fc-sub-field \{ flex: 1; \}/g,
    '.fc-sub-field { flex: 1; min-width: 100px; }\\n.fc-sub-field.field-has-rows { flex-basis: 100%; min-width: 100%; margin-bottom: 4px; }');

// Add field-has-rows class in template
content = content.replace(/<div class="fc-sub-field">/g,
    '<div class="fc-sub-field" :class="{ \\'field - has - rows\\': subProp.typeOptions?.rows }">');

fs.writeFileSync('src/components/NodeDetails.vue', content);
console.log('Fixed Vue');
