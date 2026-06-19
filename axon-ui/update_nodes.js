import fs from 'fs';
let content = fs.readFileSync('src/lib/nodes.js', 'utf8');

let oldMainGroup = `            {
                displayName: 'Main Font',
                name: 'font_path',
                type: 'options',
                options: [], // Dynamically populated from /api/fonts
                default: '',
                displayOptions: { show: { action: ['quote_image'] } },
                hint: 'Select a font for the main quote text',
            },
            {
                name: 'quote_settings_group',
                type: 'inlineGroup',
                options: [
                    {
                        displayName: 'Align',
                        name: 'alignment',
                        type: 'options',
                        options: [
                            { name: 'Left', value: 'left' },
                            { name: 'Center', value: 'center' },
                            { name: 'Right', value: 'right' },
                        ],
                        default: 'left',
                    },
                    {
                        displayName: 'Color',
                        name: 'font_color',
                        type: 'string',
                        default: '',
                        placeholder: 'Auto / #HEX',
                    },
                    {
                        displayName: 'Size',
                        name: 'font_size',
                        type: 'number',
                        default: '',
                        placeholder: 'Auto',
                    },
                ],
                displayOptions: { show: { action: ['quote_image'] } },
            },`;

let newMainGroup = `            {
                name: 'quote_settings_group',
                type: 'inlineGroup',
                options: [
                    {
                        displayName: 'Main Font',
                        name: 'font_path',
                        type: 'options',
                        options: [], // Dynamically populated from /api/fonts
                        default: '',
                        hint: 'Select a font for the main quote text',
                    },
                    {
                        displayName: 'Align',
                        name: 'alignment',
                        type: 'options',
                        options: [
                            { name: 'Left', value: 'left' },
                            { name: 'Center', value: 'center' },
                            { name: 'Right', value: 'right' },
                        ],
                        default: 'left',
                    },
                    {
                        displayName: 'Color',
                        name: 'font_color',
                        type: 'string',
                        default: '',
                        placeholder: 'Auto / #HEX',
                    },
                    {
                        displayName: 'Size',
                        name: 'font_size',
                        type: 'number',
                        default: '',
                        placeholder: 'Auto',
                    },
                ],
                displayOptions: { show: { action: ['quote_image'] } },
            },`;

content = content.replace(oldMainGroup, newMainGroup);

let oldAttrGroup = `            {
                displayName: 'Attribution Font',
                name: 'attribution_font_path',
                type: 'options',
                options: [], // Dynamically populated from /api/fonts
                default: '',
                displayOptions: { show: { action: ['quote_image'] } },
                hint: 'Select a font for the attribution text (defaults to main font if empty)',
            },
            {
                name: 'attribution_settings_group',
                type: 'inlineGroup',
                options: [
                    {
                        displayName: 'Align',
                        name: 'attribution_alignment',
                        type: 'options',
                        options: [
                            { name: 'Left', value: 'left' },
                            { name: 'Center', value: 'center' },
                            { name: 'Right', value: 'right' },
                        ],
                        default: 'left',
                    },
                    {
                        displayName: 'Color',
                        name: 'attribution_font_color',
                        type: 'string',
                        default: '',
                        placeholder: 'Auto / #HEX',
                    },
                    {
                        displayName: 'Size',
                        name: 'attribution_font_size',
                        type: 'number',
                        default: '',
                        placeholder: 'Auto',
                    },
                ],
                displayOptions: { show: { action: ['quote_image'] } },
            },`;

let newAttrGroup = `            {
                name: 'attribution_settings_group',
                type: 'inlineGroup',
                options: [
                    {
                        displayName: 'Attribution Font',
                        name: 'attribution_font_path',
                        type: 'options',
                        options: [], // Dynamically populated from /api/fonts
                        default: '',
                        hint: 'Select a font for the attribution text (defaults to main font if empty)',
                    },
                    {
                        displayName: 'Align',
                        name: 'attribution_alignment',
                        type: 'options',
                        options: [
                            { name: 'Left', value: 'left' },
                            { name: 'Center', value: 'center' },
                            { name: 'Right', value: 'right' },
                        ],
                        default: 'left',
                    },
                    {
                        displayName: 'Color',
                        name: 'attribution_font_color',
                        type: 'string',
                        default: '',
                        placeholder: 'Auto / #HEX',
                    },
                    {
                        displayName: 'Size',
                        name: 'attribution_font_size',
                        type: 'number',
                        default: '',
                        placeholder: 'Auto',
                    },
                ],
                displayOptions: { show: { action: ['quote_image'] } },
            },`;

content = content.replace(oldAttrGroup, newAttrGroup);

fs.writeFileSync('src/lib/nodes.js', content);
console.log('Updated nodes.js fields to include fonts in inlineGroups');
