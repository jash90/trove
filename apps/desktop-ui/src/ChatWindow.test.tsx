import '@testing-library/jest-dom/vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { ChatWindow, previewableLanguage, renderableMarkdown } from './ChatWindow';
import type { ChatStreamEvent } from './lib/contracts';
import { mockGateway, type ClipboardGateway } from './lib/gateway';

/// Streams like the core does: the listener the gateway registered, fed
/// event by event, the way chat-delta/chat-done would arrive.
const streamGateway = (
  overrides: {
    send?: (messages: unknown[]) => Promise<{ id: string } | never>;
  } = {},
): ClipboardGateway => {
  const listeners: ((event: ChatStreamEvent) => void)[] = [];
  const gateway: ClipboardGateway = {
    ...mockGateway,
    onChatEvent: (listener) => {
      listeners.push(listener);
      return () => {
        const index = listeners.indexOf(listener);
        if (index >= 0) listeners.splice(index, 1);
      };
    },
    chatSend: vi.fn(
      overrides.send
        ? (async (messages: unknown[]) => {
            if (overrides.send) return overrides.send(messages);
            return { id: 'turn-1' };
          })
        : (async () => {
            const id = 'turn-1';
            setTimeout(() => {
              for (const listener of [...listeners]) {
                listener({ kind: 'delta', id, part: 'answer', text: 'Hello' });
                listener({ kind: 'delta', id, part: 'answer', text: ' there' });
                listener({ kind: 'done', id });
              }
            }, 0);
            return { id };
          }),
    ),
  };
  return gateway;
};

describe('previewableLanguage', () => {
  it('names the languages that are things rather than text', () => {
    expect(previewableLanguage('html')).toBe('html');
    expect(previewableLanguage('SVG')).toBe('svg');
    expect(previewableLanguage('jsx')).toBe('jsx');
    expect(previewableLanguage('tsx')).toBe('jsx');
    expect(previewableLanguage('react')).toBe('jsx');
    expect(previewableLanguage('rust')).toBeNull();
    expect(previewableLanguage('')).toBeNull();
  });
});

describe('renderableMarkdown', () => {
  it('closes a fence that has not closed itself yet', () => {
    expect(renderableMarkdown('text\n\`\`\`rust\nfn main() {}')).toBe(
      'text\n\`\`\`rust\nfn main() {}\n\`\`\`',
    );
    // Balanced fences are left exactly as they are.
    expect(renderableMarkdown('a\n\`\`\`\ncode\n\`\`\`\nb')).toBe(
      'a\n\`\`\`\ncode\n\`\`\`\nb',
    );
  });

  it('hides a fence that is still arriving', () => {
    expect(renderableMarkdown('answer so far\n\`\`')).toBe('answer so far\n');
    expect(renderableMarkdown('answer so far\n\`')).toBe('answer so far\n');
    // A full fence line is not hidden — it opens a block.
    expect(renderableMarkdown('so far\n\`\`\`')).toBe('so far\n\`\`\`\n\`\`\`');
  });
});

describe('ChatWindow', () => {
  it('sends a message and streams the answer into the log', async () => {
    const user = userEvent.setup();
    render(<ChatWindow gateway={streamGateway()} />);

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'hi there');
    await user.keyboard('{Enter}');

    // The user's words, then the answer as it arrives, then settled.
    await waitFor(() => expect(screen.getByText('hi there')).toBeVisible());
    await waitFor(() => expect(screen.getByText('Hello there')).toBeVisible());
  });

  it('renders the answer as markdown, with copyable and saveable code', async () => {
    const user = userEvent.setup();
    const saveGeneratedFile = vi.fn(async () => true);
    const copyChatText = vi.fn(async () => undefined);
    const listeners: ((event: ChatStreamEvent) => void)[] = [];
    const gateway: ClipboardGateway = {
      ...mockGateway,
      saveGeneratedFile,
      copyChatText,
      onChatEvent: (listener) => {
        listeners.push(listener);
        return () => {
          const index = listeners.indexOf(listener);
          if (index >= 0) listeners.splice(index, 1);
        };
      },
      chatSend: vi.fn(async () => {
        const id = 'turn-md';
        setTimeout(() => {
          for (const listener of [...listeners]) {
            listener({
              kind: 'delta',
              id,
              part: 'answer',
              text: '# Notes\n\n```rust\nfn main() {}\n```',
            });
            listener({ kind: 'done', id });
          }
        }, 0);
        return { id };
      }),
    };
    render(<ChatWindow gateway={gateway} />);

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'write something');
    await user.keyboard('{Enter}');

    // The heading rendered as markdown, not as literal hashes.
    const heading = await screen.findByRole('heading', { level: 1, name: 'Notes' });
    expect(heading).toBeVisible();

    // The code block carries its two actions; save carries the code text.
    await user.click(screen.getByRole('button', { name: 'Save as file' }));
    await waitFor(() =>
      expect(saveGeneratedFile).toHaveBeenCalledWith(
        'snippet.txt',
        expect.stringContaining('fn main() {}'),
      ),
    );
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() =>
      expect(copyChatText).toHaveBeenCalledWith(expect.stringContaining('fn main() {}')),
    );
  });

  it('attaches files and sends them with the message', async () => {
    const user = userEvent.setup();
    const gateway = streamGateway();
    render(<ChatWindow gateway={gateway} />);

    const input = screen.getByLabelText('Attach files', { selector: 'input' });
    const file = new File(['fn main() {}'], 'main.rs', { type: 'text/rust' });
    await user.upload(input, file);

    await waitFor(() => expect(screen.getByText(/main\.rs/u)).toBeVisible());
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'review');
    await user.keyboard('{Enter}');

    await waitFor(() => expect(gateway.chatSend).toHaveBeenCalled());
    const chatSend = gateway.chatSend!;
    const calls = vi.mocked(chatSend).mock.calls;
    const sent = calls[0]?.[0] ?? [];
    expect(sent[sent.length - 1]?.attachments).toEqual([
      { name: 'main.rs', kind: 'text', mimeType: 'text/rust', data: 'fn main() {}' },
    ]);
  });

  it('sends multi-line messages, which the core used to refuse', async () => {
    const user = userEvent.setup();
    const gateway = streamGateway();
    render(<ChatWindow gateway={gateway} />);

    const input = screen.getByRole('textbox', { name: 'Message' });
    await user.type(input, 'first line');
    await user.keyboard('{Shift>}{Enter}{/Shift}');
    await user.type(input, 'second line');
    await user.keyboard('{Enter}');

    await waitFor(() => expect(gateway.chatSend).toHaveBeenCalled());
    const chatSend = gateway.chatSend!;
    const calls = vi.mocked(chatSend).mock.calls;
    const sent = calls[0]?.[0] ?? [];
    expect(sent[sent.length - 1]?.content ?? '').toContain('\n');
  });

  it('previews an html artifact in a sandboxed frame', async () => {
    const user = userEvent.setup();
    const listeners: ((event: ChatStreamEvent) => void)[] = [];
    const gateway: ClipboardGateway = {
      ...mockGateway,
      onChatEvent: (listener) => {
        listeners.push(listener);
        return () => {
          const index = listeners.indexOf(listener);
          if (index >= 0) listeners.splice(index, 1);
        };
      },
      chatSend: vi.fn(async () => {
        const id = 'turn-html';
        setTimeout(() => {
          for (const listener of [...listeners]) {
            listener({
              kind: 'delta',
              id,
              part: 'answer',
              text: '```html\n<h1>Live</h1>\n```',
            });
            listener({ kind: 'done', id });
          }
        }, 0);
        return { id };
      }),
    };
    render(<ChatWindow gateway={gateway} />);

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'make a page');
    await user.keyboard('{Enter}');
    const previewButton = await screen.findByRole('button', { name: /preview/iu });
    await user.click(previewButton);

    // The preview is a dialog holding a sandboxed frame: scripts run, the
    // origin does not exist, and nothing of the application is reachable.
    const dialog = screen.getByRole('dialog', { name: 'Artifact preview' });
    expect(dialog).toBeVisible();
    const frame = dialog.querySelector('iframe');
    expect(frame?.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame?.getAttribute('srcdoc')).toContain('artifact-ready');

    await user.click(screen.getByRole('button', { name: 'Close the preview' }));
    expect(screen.queryByRole('dialog', { name: 'Artifact preview' })).toBeNull();
  });

  it('says which files it cannot take, by name, instead of silence', async () => {
    const user = userEvent.setup();
    render(<ChatWindow gateway={mockGateway} />);

    const input = screen.getByLabelText('Attach files', { selector: 'input' });
    const pdf = new File(['%PDF-1.4 binary'], 'report.pdf', { type: 'application/pdf' });
    fireEvent.change(input, { target: { files: [pdf] } });

    const note = await screen.findByText(/report\.pdf/u);
    expect(note.textContent).toContain('only images and text');
    // A text file beside it still rides along.
    const txt = new File(['hello'], 'note.txt', { type: 'text/plain' });
    fireEvent.change(input, { target: { files: [txt] } });
    await waitFor(() => expect(screen.getByText(/note\.txt/u)).toBeVisible());
  });

  it('shows Shift+Enter as the newline, Enter as the send', async () => {
    const user = userEvent.setup();
    const gateway = streamGateway();
    render(<ChatWindow gateway={gateway} />);

    const input = screen.getByRole('textbox', { name: 'Message' });
    await user.type(input, 'first');
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });

    // Still one message: Shift+Enter did not send.
    expect(gateway.chatSend).not.toHaveBeenCalled();
  });

  it('settles a refusal into a sentence, not a stack or a secret', async () => {
    const user = userEvent.setup();
    const gateway = streamGateway({
      send: async () => {
        const id = 'turn-refused';
        setTimeout(() => {
          // The listener list is internal; the rejection path below is the
          // one under test here.
        }, 0);
        throw 'chat_key_unavailable';
      },
    });
    render(<ChatWindow gateway={gateway} />);

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'hello');
    await user.keyboard('{Enter}');

    await waitFor(() =>
      expect(screen.getByText(/key could not be read from the vault/i)).toBeVisible(),
    );
  });

  it('switches providers, keys and models together, and saves them', async () => {
    const user = userEvent.setup();
    const save = vi.fn(async (settings: unknown) => settings);
    const gateway: ClipboardGateway = {
      ...mockGateway,
      saveChatSettings: save as unknown as ClipboardGateway['saveChatSettings'],
    };
    render(<ChatWindow gateway={gateway} />);

    await user.click(screen.getByRole('button', { name: 'Chat settings' }));
    await user.selectOptions(screen.getByLabelText('Provider'), 'openai');
    await user.type(screen.getByLabelText('API key'), 'sk-synthetic');
    await user.selectOptions(screen.getByLabelText('Model'), 'gpt-4o');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(save).toHaveBeenCalled());
    const saved = vi.mocked(save).mock.calls[0]![0] as {
      provider: string;
      model: string;
      keys: Record<string, string>;
    };
    expect(saved.provider).toBe('openai');
    expect(saved.model).toBe('gpt-4o');
    // The key landed under its provider, not somebody else's.
    expect(saved.keys.openai).toBe('sk-synthetic');
    expect(saved.keys.zai).toBe('');
  });

  it('loads the model list from the provider on request', async () => {
    const user = userEvent.setup();
    const chatListModels = vi.fn(async () => ['glm-4.6', 'glm-5-preview']);
    const gateway: ClipboardGateway = {
      ...mockGateway,
      chatListModels,
    };
    render(<ChatWindow gateway={gateway} />);

    await user.click(screen.getByRole('button', { name: 'Chat settings' }));
    await user.click(screen.getByRole('button', { name: 'Load models' }));

    await waitFor(() => expect(chatListModels).toHaveBeenCalledOnce());
    const model = screen.getByLabelText('Model') as HTMLSelectElement;
    await waitFor(() =>
      expect(Array.from(model.options).some((option) => option.value === 'glm-5-preview')).toBe(
        true,
      ),
    );
  });

  it('stops the answer while it streams', async () => {
    const user = userEvent.setup();
    const stop = vi.fn(async () => true);
    // A turn that starts and never settles: the honest shape of a long
    // answer, which is exactly what Stop exists for.
    const gateway: ClipboardGateway = {
      ...mockGateway,
      chatStop: stop,
      chatSend: vi.fn(async () => ({ id: 'turn-hanging' })),
      onChatEvent: () => () => undefined,
    };
    render(<ChatWindow gateway={gateway} />);

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'go');
    await user.keyboard('{Enter}');

    // While the turn is in flight the composer shows Stop, not Send.
    const stopButton = await screen.findByRole('button', { name: 'Stop the answer' });
    await user.click(stopButton);
    expect(stop).toHaveBeenCalledWith('turn-hanging');
  });
});
