import js from '@eslint/js';
import importPlugin from 'eslint-plugin-import';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';
import globals from 'globals';
import tseslint from 'typescript-eslint';

const featureNames = ['room-creation', 'presenter', 'viewer'];
const crossFeatureZones = featureNames.map((feature) => ({
  target: `./src/features/${feature}`,
  from: './src/features',
  except: [`./${feature}`],
}));

export default tseslint.config(
  { ignores: ['dist', 'playwright-report', 'test-results', 'src/generated', 'eslint.config.js', 'scripts/*.mjs'] },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser, ...globals.node },
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    plugins: { import: importPlugin, 'react-hooks': reactHooks, 'react-refresh': reactRefresh },
    settings: {
      'import/resolver': { typescript: { project: './tsconfig.app.json' } },
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
      'import/no-cycle': 'error',
      'import/no-restricted-paths': ['error', {
        zones: [
          ...crossFeatureZones,
          { target: './src/features', from: './src/app' },
          {
            target: ['./src/components', './src/config', './src/hooks', './src/lib', './src/types', './src/utils'],
            from: ['./src/features', './src/app'],
          },
        ],
      }],
      '@typescript-eslint/consistent-type-imports': ['error', { prefer: 'type-imports' }],
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': 'error',
    },
  },
  {
    files: ['**/*.test.{ts,tsx}', 'src/testing/**/*.{ts,tsx}'],
    rules: { '@typescript-eslint/unbound-method': 'off' },
  },
);
