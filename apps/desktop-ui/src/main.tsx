/// <reference types="vite/client" />

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import '@fontsource-variable/jetbrains-mono';
import '@fontsource-variable/source-serif-4';
import '@fontsource-variable/space-grotesk';

import { App } from './App';
import { ChatWindow } from './ChatWindow';
import { SettingsWindow } from './SettingsWindow';
import './styles/tokens.css';
import './styles/global.css';
import './styles/palette.css';

/// Which window this document is.
///
/// Settings live in their own OS window rather than over the palette, and both
/// are served by the same bundle: the fragment says which one to render. One
/// entry point keeps the build a single page and the fonts loaded once.
const windowHash = window.location.hash;
const isSettingsWindow = windowHash === '#settings';
const isChatWindow = windowHash === '#chat';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {isSettingsWindow ? <SettingsWindow /> : isChatWindow ? <ChatWindow /> : <App />}
  </StrictMode>,
);
