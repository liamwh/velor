import js from '@eslint/js';
import tsESLint from 'typescript-eslint';
import svelteESLint from 'eslint-plugin-svelte';
import prettier from 'eslint-config-prettier';
import globals from 'globals';

/** @type {import('eslint').Linter.Config[]} */
export default [
	js.configs.recommended,
	...tsESLint.configs.recommended,
	...svelteESLint.configs['flat/recommended'],
	prettier,
	{
		languageOptions: {
			globals: {
				...globals.browser,
				...globals.node,
				...globals['es2021']
			},
			parserOptions: {
				ecmaVersion: 'latest',
				sourceType: 'module',
				extraFileExtensions: ['.svelte']
			}
		},
		rules: {
			// TypeScript specific rules
			'@typescript-eslint/no-explicit-any': 'warn',
			'@typescript-eslint/no-unused-vars': [
				'warn',
				{
					argsIgnorePattern: '^_',
					varsIgnorePattern: '^_',
					caughtErrorsIgnorePattern: '^_'
				}
			],

			// General rules - focus on bugs, not style
			'no-console': 'off',
			'no-debugger': 'warn',
			'no-unused-vars': 'off', // Use TypeScript version
			'prefer-const': 'off', // Too noisy for Svelte components
			'no-useless-assignment': 'warn',

			// Svelte navigation
			'svelte/no-navigation-without-resolve': 'off' // Using href is fine for external links
		}
	},
	{
		files: ['**/*.svelte'],
		languageOptions: {
			parserOptions: {
				parser: tsESLint.parser
			}
		},
		rules: {
			'svelte/require-each-key': 'warn',
			'svelte/no-useless-mustaches': 'warn'
		}
	},
	// Disable most rules for auto-generated shadcn-svelte UI components
	{
		files: ['src/lib/components/ui/**/*.svelte'],
		rules: {
			'@typescript-eslint/no-unused-vars': 'off',
			'svelte/no-at-html-tags': 'off',
			'svelte/valid-compile': 'warn',
			'svelte/no-navigation-without-resolve': 'off'
		}
	},
	{
		ignores: [
			'**/*.svelte.ts', // Svelte context files with runes syntax that TS parser doesn't understand
			'build/',
			'.svelte-kit/',
			'dist/',
			'node_modules/',
			'src-tauri/target/'
		]
	}
];
