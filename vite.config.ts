import { defineConfig } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import packageInfo from './package.json';
export default defineConfig({define:{__APP_VERSION__:JSON.stringify(packageInfo.version)},plugins:[tailwindcss()],clearScreen:false,server:{port:1420,strictPort:true},build:{outDir:'web-dist'}});
