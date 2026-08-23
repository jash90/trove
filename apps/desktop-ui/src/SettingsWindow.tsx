import { getCurrentWindow } from '@tauri-apps/api/window';

import { SettingsPanel } from './components/SettingsPanel';
import { GatewayProvider, useGateway } from './lib/gateway';

/// Closes this window, or does nothing outside the desktop shell.
///
/// The browser preview has no window to close, and throwing there would break
/// the one place the settings can be looked at without building the app.
const closeThisWindow = (): void => {
  try {
    void getCurrentWindow().close();
  } catch {
    // Not running inside the shell.
  }
};

const SettingsRoot = (): React.JSX.Element => (
  <SettingsPanel gateway={useGateway()} onClose={closeThisWindow} />
);

export const SettingsWindow = (): React.JSX.Element => (
  <GatewayProvider>
    <SettingsRoot />
  </GatewayProvider>
);
