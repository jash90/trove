/// <reference types="vite/client" />

import { lazy, StrictMode, Suspense } from 'react';
import { createRoot } from 'react-dom/client';

import '@fontsource-variable/jetbrains-mono';
import '@fontsource-variable/source-serif-4';
import '@fontsource-variable/space-grotesk';

import { App } from './App';
import './styles/tokens.css';
import './styles/global.css';
import './styles/palette.css';

/// The two windows that are not the palette, loaded only by the window that
/// shows them.
///
/// All three windows load this one entry point, and the palette is the one a
/// launch waits on. Imported statically, chat brought the markdown renderer
/// with it and settings its whole form, so the palette parsed more than half a
/// megabyte of code it never runs before it could draw. Split out, each window
/// loads what it renders.
const ChatWindow = lazy(() =>
  import('./ChatWindow').then((module) => ({ default: module.ChatWindow })),
);
const SettingsWindow = lazy(() =>
  import('./SettingsWindow').then((module) => ({ default: module.SettingsWindow })),
);

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
    {isSettingsWindow ? (
      // Nothing as the fallback: the chunk is local and arrives within a
      // frame or two, and a placeholder flashing first would be all anyone saw.
      <Suspense fallback={null}>
        <SettingsWindow />
      </Suspense>
    ) : isChatWindow ? (
      <Suspense fallback={null}>
        <ChatWindow />
      </Suspense>
    ) : (
      <App />
    )}
  </StrictMode>,
);
