import pluginVue from 'eslint-plugin-vue'
import globals from 'globals'

export default [
  {
    ignores: ['dist/**', 'node_modules/**'],
  },
  ...pluginVue.configs['flat/recommended'],
  {
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
      globals: {
        ...globals.browser,
        ...globals.node,
      },
    },
    rules: {
      // Vue Flow node/edge components and a few premium-UI patterns use
      // multi-word-looking names that are still fine as single-file
      // component names in this codebase (e.g. page-level components).
      'vue/multi-word-component-names': 'off',
    },
  },
]
