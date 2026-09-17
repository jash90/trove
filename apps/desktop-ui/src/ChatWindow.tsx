import { ArrowUp, Download, MessageSquareText, Paperclip, Play, Settings2, Square, Trash2, X } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkBreaks from 'remark-breaks';
import remarkGfm from 'remark-gfm';
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEventHandler,
} from 'react';

import { ARTIFACT_BOOTSTRAP } from './lib/artifactBootstrap';
import {
  CHAT_PROVIDERS,
  chatKeyField,
  type ChatAttachment,
  type ChatMessage,
  type ChatProvider,
  type ChatRole,
  type ChatSettings,
  type ChatStreamEvent,
} from './lib/contracts';
import { GatewayProvider, useGateway, type ClipboardGateway } from './lib/gateway';

/** The interface's copy for the core's stable refusal codes. */
const CHAT_ERROR_SENTENCES: Record<string, string> = {
  chat_key_unavailable:
    'The key could not be read from the vault. Check that it is paired and the slug is right.',
  chat_network_error: 'The provider could not be reached. Check the connection and the base URL.',
  chat_provider_refused:
    'The provider refused the request. Check the model name and the key.',
  chat_key_missing: 'Add this provider\u2026s API key in the chat settings first.',
  chat_key_refused:
    'The provider refused this key. Check it — and if it is a Z.ai Coding Plan key, choose the Coding Plan provider, whose endpoint is its own.',
  chat_endpoint_not_found:
    'The provider has no such model or endpoint. Check the model name, or load the list again.',
  chat_rate_limited: 'The provider is rate-limiting. Wait a moment and try again.',
  chat_models_unavailable:
    'The model list could not be fetched. The suggestions stand; type on.',
  chat_invalid_request: 'That message could not be sent.',
  chat_stream_ended_empty: 'The provider closed the stream without answering.',
  chat_stream_malformed: 'The provider sent a stream this window cannot read.',
  chat_event_unavailable: 'The answer could not be delivered to the window.',
  chat_settings_unavailable: 'The chat settings could not be read.',
};

const sentenceForCode = (code: string): string =>
  CHAT_ERROR_SENTENCES[code] ?? 'The model could not be reached. Try again in a moment.';

interface ChatDisplayMessage {
  role: ChatRole;
  content: string;
  /** The model's thinking, streamed before its answer. */
  reasoning: string;
  /** While the answer streams in. */
  pending: boolean;
  /** Set when the turn refused; the sentence replaces the content. */
  error: string | null;
  /** What the user attached, shown as chips on the message. */
  attachments: ChatAttachment[];
}

interface ChatWindowProps {
  gateway?: ClipboardGateway;
}

const ChatConversation = (): React.JSX.Element => {
  const gateway = useGateway();
  const capable =
    gateway.chatSend !== undefined &&
    gateway.getChatSettings !== undefined &&
    gateway.saveChatSettings !== undefined;

  const [messages, setMessages] = useState<ChatDisplayMessage[]>([]);
  const [draft, setDraft] = useState('');
  const [settings, setSettings] = useState<ChatSettings | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [turnId, setTurnId] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<ChatAttachment[]>([]);
  const [refusedFiles, setRefusedFiles] = useState<string[]>([]);
  const [preview, setPreview] = useState<{ language: 'html' | 'svg' | 'jsx'; source: string } | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    void gateway
      .getChatSettings?.()
      .then(setSettings)
      .catch(() => undefined);
  }, [gateway]);

  // The stream arrives as events carrying the turn id; deltas append to the
  // pending answer, the settle closes it — a refusal replacing it with a
  // sentence that names what went wrong without quoting the provider.
  useEffect(() => {
    if (gateway.onChatEvent === undefined) return;
    return gateway.onChatEvent((event: ChatStreamEvent) => {
      setMessages((previous) => {
        if (previous.length === 0) return previous;
        const last = previous[previous.length - 1]!;
        if (!last.pending || last.role !== 'assistant') return previous;
        if ('id' in event && turnRef.current !== null && event.id !== turnRef.current) {
          return previous;
        }
        const next = [...previous];
        if (event.kind === 'delta') {
          // The two voices of a reasoning model: the thinking gathers in
          // its own field, the answer in the content — each rendered as
          // what it is.
          next[next.length - 1] =
            event.part === 'reasoning'
              ? { ...last, reasoning: last.reasoning + event.text }
              : { ...last, content: last.content + event.text };
        } else if (event.kind === 'done') {
          next[next.length - 1] = { ...last, pending: false };
        } else {
          next[next.length - 1] = {
            ...last,
            pending: false,
            error: sentenceForCode(event.code),
          };
        }
        return next;
      });
      if (event.kind === 'done' || event.kind === 'error') setTurnId(null);
    });
  }, [gateway]);

  const turnRef = useRef<string | null>(null);
  useEffect(() => {
    turnRef.current = turnId;
  }, [turnId]);

  // The list follows the answer as it grows, the way every chat does.
  useEffect(() => {
    // jsdom has no scrollTo; the guard is the same one the palette's list uses.
    if (typeof listRef.current?.scrollTo === 'function') {
      listRef.current.scrollTo({ top: listRef.current.scrollHeight });
    }
  }, [messages]);

  const send = useCallback((): void => {
    const content = draft.trim();
    if ((content === '' && attachments.length === 0) || turnId !== null || !capable) return;
    const riding = attachments;
    const conversation: ChatMessage[] = [
      ...messages
        .filter((message) => message.error === null)
        .map((message): ChatMessage => ({
          role: message.role,
          content: message.content,
          // Older messages send no attachments: the model has seen them,
          // and resending megabytes with every turn serves nobody.
        })),
      { role: 'user', content: content === '' ? '(see attachments)' : content, attachments: riding },
    ];
    setMessages((previous) => [
      ...previous,
      {
        role: 'user',
        content,
        reasoning: '',
        pending: false,
        error: null,
        attachments: riding,
      },
      { role: 'assistant', content: '', reasoning: '', pending: true, error: null, attachments: [] },
    ]);
    setDraft('');
    setAttachments([]);
    void gateway
      .chatSend?.(conversation)
      .then((turn) => setTurnId(turn.id))
      .catch((reason: unknown) => {
        const code = typeof reason === 'string' ? reason : 'chat_invalid_request';
        setMessages((previous) => {
          const next = [...previous];
          const last = next[next.length - 1];
          if (last)
            next[next.length - 1] = {
              ...last,
              pending: false,
              error: sentenceForCode(code),
            };
          return next;
        });
        setTurnId(null);
      });
    inputRef.current?.focus();
  }, [draft, attachments, messages, turnId, capable, gateway]);

  /// What a file can be told it cannot be: the chat wires carry text and
  /// images and nothing else — a PDF has no inline lane on chat
  /// completions — so a file outside that is said so, by name, rather than
  /// dropped in silence that reads as "images only".
  const isTextFile = (file: File): boolean =>
    file.type.startsWith('text/') ||
    /\.(md|txt|json|csv|ya?ml|toml|rs|ts|tsx|js|jsx|py|go|java|c|h|cpp|sh|html|css|sql|swift|kt)$/iu.test(
      file.name,
    );

  const readAttachedFiles = (files: FileList | null): void => {
    if (files === null) return;
    const refused: string[] = [];
    for (const file of Array.from(files).slice(0, 4)) {
      if (file.type.startsWith('image/')) {
        if (file.size > 3 * 1024 * 1024) {
          refused.push(`${file.name} is over 3 MB`);
          continue;
        }
        const reader = new FileReader();
        reader.onload = () => {
          const dataUrl = String(reader.result ?? '');
          const base64 = dataUrl.slice(dataUrl.indexOf(',') + 1);
          setAttachments((previous) => [
            ...previous.slice(0, 3),
            { name: file.name, kind: 'image', mimeType: file.type, data: base64 },
          ]);
        };
        reader.readAsDataURL(file);
      } else if (isTextFile(file)) {
        if (file.size > 256 * 1024) {
          refused.push(`${file.name} is over 256 KB`);
          continue;
        }
        const reader = new FileReader();
        reader.onload = () => {
          setAttachments((previous) => [
            ...previous.slice(0, 3),
            { name: file.name, kind: 'text', mimeType: file.type || 'text/plain', data: String(reader.result ?? '') },
          ]);
        };
        reader.readAsText(file);
      } else {
        refused.push(`${file.name} — only images and text files can ride along`);
      }
    }
    if (refused.length > 0) {
      setRefusedFiles(refused);
      setTimeout(() => setRefusedFiles([]), 6000);
    }
  };

  const stop = useCallback((): void => {
    if (turnId === null) return;
    void gateway.chatStop?.(turnId).catch(() => undefined);
  }, [turnId, gateway]);

  const handleDraftKeyDown: KeyboardEventHandler<HTMLTextAreaElement> = (event) => {
    // Enter sends; Shift+Enter is the newline, the way every chat field
    // behaves. Modifiers pass through untouched.
    if (event.key === 'Enter' && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault();
      send();
    }
  };

  const saveSettings = (next: ChatSettings): void => {
    void gateway
      .saveChatSettings?.(next)
      .then((saved) => {
        setSettings(saved);
        setSettingsOpen(false);
      })
      .catch(() => undefined);
  };

  const streaming = turnId !== null;
  const modelLabel = useMemo(() => {
    if (settings === null) return '…';
    const provider = CHAT_PROVIDERS.find((entry) => entry.id === settings.provider);
    return `${provider?.label ?? settings.provider} · ${settings.model}`;
  }, [settings]);

  return (
    <main className="chat-shell" aria-label="Trove chat">
      <header className="chat-header">
        <MessageSquareText size={16} strokeWidth={1.8} aria-hidden="true" />
        <span className="chat-header__model">{modelLabel}</span>
        <span className="chat-header__actions">
          <button
            type="button"
            className="chat-icon-button"
            aria-label="Chat settings"
            aria-expanded={settingsOpen}
            onClick={() => setSettingsOpen((open) => !open)}
          >
            <Settings2 size={15} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="chat-icon-button"
            aria-label="Save the conversation as Markdown"
            disabled={messages.length === 0}
            onClick={() => {
              const markdown = messages
                .map((message) =>
                  message.error !== null
                    ? `> ${message.error}`
                    : `${message.role === 'user' ? '## You' : '## Model'}\n\n${message.content}`,
                )
                .join('\n\n---\n\n');
              void gateway
                .saveGeneratedFile?.('trove-chat.md', markdown)
                .catch(() => undefined);
            }}
          >
            <Download size={15} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="chat-icon-button"
            aria-label="Clear the conversation"
            disabled={messages.length === 0 || streaming}
            onClick={() => setMessages([])}
          >
            <Trash2 size={15} aria-hidden="true" />
          </button>
        </span>
      </header>

      {settingsOpen && settings !== null ? (
        <ChatSettingsForm settings={settings} onSave={saveSettings} gateway={gateway} />
      ) : null}

      <div className="chat-log" ref={listRef} role="log" aria-live="polite">
        {messages.length === 0 ? (
          <p className="chat-empty">
            A conversation with the model the settings name. The key is given in
            the settings here and stored in this application; the conversation
            goes only to the provider named above.
          </p>
        ) : (
          messages.map((message, index) => (
            <article
              key={index}
              className={`chat-message chat-message--${message.role}${
                message.pending ? ' is-pending' : ''
              }`}
            >
              {message.attachments.length > 0 ? (
                <p className="chat-message__files">
                  {message.attachments.map((attachment) => (
                    <span key={attachment.name} className="chat-chip">
                      {attachment.kind === 'image' ? '🖼 ' : '📄 '}
                      {attachment.name}
                    </span>
                  ))}
                </p>
              ) : null}
              {message.role === 'assistant' && message.reasoning !== '' ? (
                <details
                  className="chat-thinking"
                  // Open exactly while there is nothing else to read: the
                  // model thinking is the answer's progress bar until the
                  // answer itself starts arriving.
                  open={message.pending && message.content === ''}
                >
                  <summary>Thinking</summary>
                  <p className="chat-thinking__text">{message.reasoning}</p>
                </details>
              ) : null}
              {message.role === 'assistant' && message.error === null ? (
                <div className="chat-message__content chat-markdown">
                  <ChatMarkdown
                    gateway={gateway}
                    text={message.content}
                    onPreview={setPreview}
                  />
                  {message.pending && message.content === '' && message.reasoning === '' ? (
                    <span className="chat-message__waiting" aria-label="Thinking">
                      …
                    </span>
                  ) : null}
                </div>
              ) : (
                <p className="chat-message__content">
                  {message.error ?? message.content}
                  {message.pending && message.content === '' ? (
                    <span className="chat-message__waiting" aria-label="Thinking">
                      …
                    </span>
                  ) : null}
                </p>
              )}
            </article>
          ))
        )}
      </div>

      <footer className="chat-composer">
        {refusedFiles.length > 0 ? (
          <p className="chat-composer__refused" role="status">
            {refusedFiles.join(' · ')}
          </p>
        ) : null}
        {attachments.length > 0 ? (
          <p className="chat-composer__files">
            {attachments.map((attachment, index) => (
              <span key={`${attachment.name}-${index}`} className="chat-chip">
                {attachment.kind === 'image' ? '🖼 ' : '📄 '}
                {attachment.name}
                <button
                  type="button"
                  className="chat-chip__remove"
                  aria-label={`Remove ${attachment.name}`}
                  onClick={() =>
                    setAttachments((previous) =>
                      previous.filter((_, position) => position !== index),
                    )
                  }
                >
                  <X size={11} aria-hidden="true" />
                </button>
              </span>
            ))}
          </p>
        ) : null}
        <div className="chat-composer__row">
        <button
          type="button"
          className="chat-icon-button"
          aria-label="Attach files"
          disabled={!capable || attachments.length >= 4}
          onClick={() => fileInputRef.current?.click()}
        >
          <Paperclip size={15} aria-hidden="true" />
        </button>
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="sr-only"
          aria-label="Attach files"
          tabIndex={-1}
          accept="image/*,.md,.txt,.json,.csv,.yaml,.yml,.toml,.rs,.ts,.tsx,.js,.jsx,.py,.go,.java,.c,.h,.cpp,.sh,.html,.css,.sql,.swift,.kt"
          onChange={(event) => {
            readAttachedFiles(event.currentTarget.files);
            event.currentTarget.value = '';
          }}
        />
        <textarea
          ref={inputRef}
          className="chat-composer__input"
          rows={3}
          placeholder={capable ? 'Write to the model…' : 'Chat is unavailable in this window.'}
          value={draft}
          disabled={!capable}
          aria-label="Message"
          onChange={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={handleDraftKeyDown}
        />
        {streaming ? (
          <button
            type="button"
            className="chat-composer__send"
            aria-label="Stop the answer"
            onClick={stop}
          >
            <Square size={15} aria-hidden="true" />
          </button>
        ) : (
          <button
            type="button"
            className="chat-composer__send"
            aria-label="Send the message"
            disabled={draft.trim() === '' || !capable}
            onClick={send}
          >
            <ArrowUp size={15} aria-hidden="true" />
          </button>
        )}
        </div>
      </footer>
      {preview !== null ? (
        <ArtifactPreview preview={preview} onClose={() => setPreview(null)} />
      ) : null}
    </main>
  );
};

/**
 * Which fenced languages are worth a live preview, and under which name.
 * Everything else renders as code and nothing more — the list is the
 * boundary between "an answer that contains code" and "an answer that is
 * a thing".
 */
export const previewableLanguage = (info: string): 'html' | 'svg' | 'jsx' | null => {
  const normalized = info.trim().toLocaleLowerCase('en-US');
  if (normalized === 'html' || normalized === 'svg') return normalized;
  if (normalized === 'jsx' || normalized === 'tsx' || normalized === 'react') return 'jsx';
  return null;
};

/// One live preview: the sandbox document in an iframe, with the artifact
/// posted to it once it says it is listening.
///
/// `allow-scripts` and never `allow-same-origin`: the frame runs what the
/// model wrote, which is exactly as trustworthy as anything else the
/// model says — an opaque origin keeps that between the preview and
/// itself. The document is an inline classic script, so it loads nowhere
/// and asks nothing: no origin to be allowed, no module to be checked.
/// JSX is compiled here, in the main window, by a Babel chunk that loads
/// on first preview; only the compiled result crosses into the sandbox,
/// where a minimal React runs it.
const ArtifactPreview = ({
  preview,
  onClose,
}: {
  preview: { language: 'html' | 'svg' | 'jsx'; source: string };
  onClose: () => void;
}): React.JSX.Element => {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const pending = useRef(preview);
  pending.current = preview;
  const [compileState, setCompileState] = useState<
    { phase: 'idle' } | { phase: 'compiling' } | { phase: 'ready' } | { phase: 'failed'; note: string }
  >({ phase: 'idle' });

  const send = useCallback((): void => {
    const frame = frameRef.current;
    if (frame === null) return;
    const current = pending.current;
    if (current.language !== 'jsx') {
      frame.contentWindow?.postMessage(
        { kind: 'artifact', language: current.language, source: current.source },
        '*',
      );
      return;
    }
    setCompileState({ phase: 'compiling' });
    void import('@babel/standalone')
      .then((Babel) => {
        let compiled: string;
        try {
          compiled =
            Babel.transform(current.source, {
              presets: [['react', { runtime: 'classic' }], 'typescript'],
              plugins: ['transform-modules-commonjs'],
              filename: 'artifact.tsx',
            })?.code ?? '';
        } catch (error) {
          const note = `The JSX did not compile:\n\n${String(error)}`;
          frame.contentWindow?.postMessage(
            { kind: 'artifact', language: 'jsx', error: note },
            '*',
          );
          setCompileState({ phase: 'failed', note });
          return;
        }
        frame.contentWindow?.postMessage(
          { kind: 'artifact', language: 'jsx', compiled },
          '*',
        );
        setCompileState({ phase: 'ready' });
      })
      .catch(() => setCompileState({ phase: 'failed', note: 'The compiler could not be loaded.' }));
  }, []);

  useEffect(() => {
    const onMessage = (event: MessageEvent): void => {
      if ((event.data as { kind?: string } | null)?.kind === 'artifact-ready') send();
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, [send]);

  return (
    <div className="chat-artifact" role="dialog" aria-label="Artifact preview">
      <header className="chat-artifact__bar">
        <span className="chat-artifact__title">{preview.language}</span>
        <span className="chat-artifact__note">
          {compileState.phase === 'compiling'
            ? 'Compiling…'
            : compileState.phase === 'failed'
              ? 'The JSX did not compile'
              : 'Runs sandboxed — nothing of this application is reachable'}
        </span>
        <button
          type="button"
          className="chat-icon-button"
          aria-label="Close the preview"
          onClick={onClose}
        >
          <X size={15} aria-hidden="true" />
        </button>
      </header>
      <iframe
        ref={frameRef}
        className="chat-artifact__frame"
        title="Artifact preview"
        sandbox="allow-scripts"
        srcDoc={ARTIFACT_BOOTSTRAP}
        onLoad={send}
      />
    </div>
  );
};

/**
 * What a half-streamed answer renders as.
 *
 * A stream delivers markdown split anywhere a chunk happened to break,
 * and two shapes of "half" render as artifacts rather than content:
 * a fence line that has not finished arriving (\`\`\` alone is not a
 * fence yet, so it shows as literal backticks), and a fence that opened
 * and never closed (fine by CommonMark, but the closing one belongs
 * before anything the model says next). Both are repaired for rendering
 * only — the text itself is never edited.
 */
export const renderableMarkdown = (text: string): string => {
  // Drop a trailing run of backticks shorter than a fence: it is a fence
  // still arriving, not content.
  const withoutPartialFence = text.replace(/(?<!`)`{1,2}$/u, '');
  const fences = (withoutPartialFence.match(/```/gu) ?? []).length;
  return fences % 2 === 1 ? `${withoutPartialFence}\n\`\`\`` : withoutPartialFence;
};

/// The markdown an answer is written in, rendered locally — no network, no
/// raw HTML: react-markdown skips markup by default, and the links go
/// through the core's opener, which decides the scheme. Single newlines
/// break lines, the way every chat field writes and every chat reads.
const ChatMarkdown = ({
  gateway,
  text,
  onPreview,
}: {
  gateway: ClipboardGateway;
  text: string;
  onPreview: (preview: { language: 'html' | 'svg' | 'jsx'; source: string }) => void;
}): React.JSX.Element => (
  <ReactMarkdown
    remarkPlugins={[remarkGfm, remarkBreaks]}
    components={{
      a: ({ href, children }) => (
        <a
          href={href}
          onClick={(event) => {
            event.preventDefault();
            if (typeof href === 'string') {
              void gateway.openExternalUrl?.(href).catch(() => undefined);
            }
          }}
        >
          {children}
        </a>
      ),
      pre: ({ children }) => (
        <CodeBlock gateway={gateway} node={children} onPreview={onPreview} />
      ),
    }}
  >
    {renderableMarkdown(text)}
  </ReactMarkdown>
);

/// One fenced block of an answer, with what a block can be asked for: a
/// copy, a file on disk, and — for the languages that are things rather
/// than text — a live preview.
const CodeBlock = ({
  gateway,
  node,
  onPreview,
}: {
  gateway: ClipboardGateway;
  node: React.ReactNode;
  onPreview: (preview: { language: 'html' | 'svg' | 'jsx'; source: string }) => void;
}): React.JSX.Element => {
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);
  const text = extractText(node);
  const language = previewableLanguage(classNameOf(node));
  return (
    <div className="chat-code">
      <div className="chat-code__toolbar">
        {language !== null ? (
          <button
            type="button"
            className="chat-code__action chat-code__action--run"
            onClick={() => onPreview({ language, source: text })}
          >
            <Play size={11} aria-hidden="true" /> Preview
          </button>
        ) : null}
        <button
          type="button"
          className="chat-code__action"
          onClick={() => {
            void gateway
              .copyChatText?.(text)
              .then(() => {
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              })
              .catch(() => undefined);
          }}
        >
          {copied ? 'Copied' : 'Copy'}
        </button>
        <button
          type="button"
          className="chat-code__action"
          onClick={() => {
            void gateway
              .saveGeneratedFile?.('snippet.txt', text)
              .then((wrote) => {
                setSaved(wrote);
                setTimeout(() => setSaved(false), 1500);
              })
              .catch(() => undefined);
          }}
        >
          {saved ? 'Saved' : 'Save as file'}
        </button>
      </div>
      <pre>{node}</pre>
    </div>
  );
};

/// The language of a fenced block, from the code element react-markdown
/// renders inside the pre: `language-rust` → `rust`.
const classNameOf = (node: React.ReactNode): string => {
  if (node !== null && typeof node === 'object' && 'props' in node) {
    const props = (node as { props?: { className?: unknown } }).props;
    if (typeof props?.className === 'string') return props.className.replace(/^language-/u, '');
  }
  return '';
};

const extractText = (node: React.ReactNode): string => {
  if (typeof node === 'string') return node;
  if (typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(extractText).join('');
  if (node !== null && typeof node === 'object' && 'props' in node) {
    const props = (node as { props?: { children?: React.ReactNode } }).props;
    return extractText(props?.children);
  }
  return '';
};

const ChatSettingsForm = ({
  settings,
  onSave,
  gateway,
}: {
  settings: ChatSettings;
  onSave: (next: ChatSettings) => void;
  gateway: ClipboardGateway;
}): React.JSX.Element => {
  const [provider, setProvider] = useState<ChatProvider>(settings.provider);
  const [model, setModel] = useState(settings.model);
  const [keys, setKeys] = useState(settings.keys);
  const [fetched, setFetched] = useState<string[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchFailed, setFetchFailed] = useState(false);

  const providerEntry = CHAT_PROVIDERS.find((entry) => entry.id === provider);
  const suggestions = providerEntry?.suggestedModels ?? [];
  // The fetched list leads once it exists; the suggestions stand beside it
  // so a provider whose list endpoint refuses is still usable, and the
  // current model is always an option even when neither list names it.
  const options = Array.from(
    new Set([
      ...fetched,
      ...suggestions,
      ...(model === '' ? [] : [model]),
    ]),
  ).sort((a, b) => a.localeCompare(b, 'en', { numeric: true }));

  const loadModels = (): void => {
    if (fetching) return;
    setFetching(true);
    setFetchFailed(false);
    void gateway
      .chatListModels?.()
      .then((models) => {
        setFetched(models);
        setFetching(false);
      })
      .catch(() => {
        setFetchFailed(true);
        setFetching(false);
      });
  };

  const switchProvider = (next: ChatProvider): void => {
    setProvider(next);
    setFetched([]);
    setFetchFailed(false);
    // A model belongs to its provider: switching lands on that provider's
    // first suggestion rather than a name the new one would refuse.
    const entry = CHAT_PROVIDERS.find((candidate) => candidate.id === next);
    const first = entry?.suggestedModels[0];
    setModel(first ?? '');
  };

  return (
    <form
      className="chat-settings"
      onSubmit={(event) => {
        event.preventDefault();
        onSave({
          provider,
          model: model.trim(),
          keys: {
            zai: keys.zai.trim(),
            openai: keys.openai.trim(),
            openrouter: keys.openrouter.trim(),
            anthropic: keys.anthropic.trim(),
          },
        });
      }}
    >
      <label className="chat-settings__field">
        <span>Provider</span>
        <select
          value={provider}
          onChange={(event) => switchProvider(event.currentTarget.value as ChatProvider)}
        >
          {CHAT_PROVIDERS.map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.label}
            </option>
          ))}
        </select>
      </label>
      <label className="chat-settings__field">
        <span>API key</span>
        <input
          type="password"
          value={keys[chatKeyField(provider)]}
          spellCheck={false}
          autoComplete="off"
          placeholder={`${providerEntry?.label ?? provider} key`}
          onChange={(event) => {
            // Read before the updater: the synthetic event is recycled by
            // the time React runs it, and currentTarget would be null.
            const value = event.currentTarget.value;
            const field = chatKeyField(provider);
            setKeys((previous) => ({ ...previous, [field]: value }));
          }}
        />
      </label>
      <label className="chat-settings__field">
        <span>Model</span>
        <select
          value={model}
          onChange={(event) => setModel(event.currentTarget.value)}
        >
          {model === '' ? <option value="">Pick a model…</option> : null}
          {options.map((id) => (
            <option key={id} value={id}>
              {id}
            </option>
          ))}
        </select>
      </label>
      <button
        type="button"
        className="chat-settings__load"
        onClick={loadModels}
        disabled={fetching}
      >
        {fetching ? 'Loading…' : 'Load models'}
      </button>
      {fetchFailed ? (
        <span className="chat-settings__fetch-note" role="status">
          The list could not be fetched; the suggestions stand.
        </span>
      ) : null}
      <button type="submit" className="chat-settings__save">
        Save
      </button>
    </form>
  );
};

export const ChatWindow = ({ gateway }: ChatWindowProps): React.JSX.Element => (
  <GatewayProvider gateway={gateway}>
    <ChatConversation />
  </GatewayProvider>
);
