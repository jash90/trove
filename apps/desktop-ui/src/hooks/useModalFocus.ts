import { useEffect, type KeyboardEventHandler, type RefObject } from 'react';

const FOCUSABLE_SELECTOR = [
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[href]',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

interface UseModalFocusOptions {
  active: boolean;
  initialFocusRef: RefObject<HTMLElement | null>;
  returnFocusRef?: RefObject<HTMLElement | null>;
  /**
   * Read at close time. When it resolves to true the invoker keeps focus,
   * which is wrong after a destructive action removed what the invoker acted on.
   */
  suppressReturnFocusRef?: RefObject<boolean>;
  focusKey?: string;
}

interface UseModalFocusResult {
  onKeyDown: KeyboardEventHandler<HTMLElement>;
}

export const useModalFocus = ({
  active,
  initialFocusRef,
  returnFocusRef,
  suppressReturnFocusRef,
  focusKey = 'initial',
}: UseModalFocusOptions): UseModalFocusResult => {
  useEffect(() => {
    if (!active) return;
    initialFocusRef.current?.focus();
  }, [active, focusKey, initialFocusRef]);

  useEffect(() => {
    if (!active) return;
    const returnTarget = returnFocusRef?.current ?? document.activeElement;
    return () => {
      if (suppressReturnFocusRef?.current === true) return;
      if (returnTarget instanceof HTMLElement && returnTarget.isConnected) {
        returnTarget.focus();
      }
    };
  }, [active, returnFocusRef, suppressReturnFocusRef]);

  const onKeyDown: KeyboardEventHandler<HTMLElement> = (event) => {
    event.stopPropagation();
    if (event.key !== 'Tab') return;
    const focusable = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter((element) => !element.hasAttribute('hidden'));
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1)!;
    if (event.shiftKey && (document.activeElement === first || !focusable.includes(document.activeElement as HTMLElement))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return { onKeyDown };
};
