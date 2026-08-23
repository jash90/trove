/// <reference types="vite/client" />

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import '@fontsource-variable/jetbrains-mono';
import '@fontsource-variable/source-serif-4';
import '@fontsource-variable/space-grotesk';

import { App } from './App';
import './styles/tokens.css';
import './styles/global.css';
import './styles/palette.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
