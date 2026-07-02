import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import {
  attachSession,
  deleteSessionAttachment,
  detachSession,
  listSessionAttachments,
  reactivateSession,
  resizeSession,
  saveSessionAttachment,
  writeSession,
} from "../api/tauri";
import type {
  PtyDataEvent,
  SessionAttachmentInfo,
  SessionErrorEvent,
  SessionEvent,
  SessionInfo,
} from "../types";

type TerminalViewProps = {
  sessionId: string;
  cwd?: string | null;
  onPreviewFile?: (path: string) => void;
  onSessionActivated?: (session: SessionInfo) => void;
  onActivationFailed?: (sessionId: string, reason: string) => Promise<void> | void;
};

const MIN_ROWS = 5;
const MIN_COLS = 10;
const MAX_ROWS = 240;
const MAX_COLS = 600;
const SCROLLBAR_GUTTER_COLS = 2;
const IMAGE_PLACEHOLDER_PATTERN = /\[paste image (\d+)\]/gi;
const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END = "\x1b[201~";
const TERMINAL_FILE_PATH_PATTERN =
  /(?:"([^"\r\n]+\.[A-Za-z0-9]{1,12}(?::\d+(?::\d+)?)?)"|'([^'\r\n]+\.[A-Za-z0-9]{1,12}(?::\d+(?::\d+)?)?)'|((?:~|\/|\.{1,2}\/)?[A-Za-z0-9_.@%+=,~/-]+\.[A-Za-z0-9]{1,12}(?::\d+(?::\d+)?)?))/g;
const TERMINAL_PATH_TRAILING_PUNCTUATION = /[),.;\]}]+$/;


function isDirectInterceptablePrintable(key: string): boolean {
  if (key.length !== 1) {
    return false;
  }
  const code = key.charCodeAt(0);
  return code >= 0x20 && code !== 0x7f;
}

function isCjkOrFullwidthPunctuation(key: string): boolean {
  const code = key.codePointAt(0);
  if (code === undefined) {
    return false;
  }
  return (code >= 0x3000 && code <= 0x303f) || (code >= 0xff00 && code <= 0xffef);
}

function isLiveDirectInterceptableInput(value: string): boolean {
  if (value.length !== 1) {
    return false;
  }
  const code = value.charCodeAt(0);
  // Intercept basic ASCII printable characters in live mode, except Space (0x20).
  // Space is excluded because it is often used to select/confirm IME candidates,
  // and intercepting it as a literal space keydown would insert unwanted spaces
  // after IME character commitments.
  return code > 0x20 && code <= 0x7e;
}

function clampDimension(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function decodeBase64Bytes(base64: string): Uint8Array | null {
  if (!base64) {
    return null;
  }
  try {
    const binary = window.atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  } catch {
    return null;
  }
}

function isImageFile(file: File): boolean {
  return file.type.startsWith("image/");
}

function clipboardImageFiles(items: DataTransferItemList | undefined): File[] {
  if (!items) {
    return [];
  }
  return Array.from(items)
    .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
    .map((item) => item.getAsFile())
    .filter((file): file is File => Boolean(file));
}

function dataTransferHasImage(items: DataTransferItemList | undefined): boolean {
  if (!items) {
    return false;
  }
  return Array.from(items).some((item) => item.kind === "file" && item.type.startsWith("image/"));
}

async function fileToBase64(file: File): Promise<string> {
  const buffer = await file.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    const chunk = bytes.subarray(offset, offset + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return window.btoa(binary);
}

function isFocusOrMouseSequence(data: string): boolean {
  return (
    data === "\x1b[I" ||
    data === "\x1b[O" ||
    data.startsWith("\x1b[M") ||
    data.startsWith("\x1b[<")
  );
}

function trimTerminalPathToken(value: string): string {
  return value
    .trim()
    .replace(TERMINAL_PATH_TRAILING_PUNCTUATION, "")
    .replace(/:\d+(?::\d+)?$/, "");
}

function resolveTerminalPath(value: string, cwd?: string | null): string {
  const token = trimTerminalPathToken(value);
  if (!token || token.startsWith("/") || token.startsWith("~") || /^[A-Za-z]:[\\/]/.test(token)) {
    return token;
  }
  if (!cwd) {
    return token;
  }
  const parts = `${cwd.replace(/\/+$/, "")}/${token}`.split("/");
  const normalized: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") {
      continue;
    }
    if (part === "..") {
      normalized.pop();
      continue;
    }
    normalized.push(part);
  }
  return `/${normalized.join("/")}`;
}

function TerminalView({ sessionId, cwd, onPreviewFile, onSessionActivated, onActivationFailed }: TerminalViewProps) {
  const shellRef = useRef<HTMLDivElement | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const commandLinkModeRef = useRef(false);
  const [status, setStatus] = useState("connecting");
  const [isRestoring, setIsRestoring] = useState(false);
  const [attachments, setAttachments] = useState<SessionAttachmentInfo[]>([]);
  const [isSavingAttachment, setIsSavingAttachment] = useState(false);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const activateAndQueueRef = useRef<((data: string) => void) | null>(null);
  const pushInputRef = useRef<((data: string) => void) | null>(null);
  const placeholderSlotByAttachmentIdRef = useRef<Map<string, number>>(new Map());
  const attachmentPathByPlaceholderSlotRef = useRef<Map<number, string>>(new Map());
  const nextPlaceholderSlotRef = useRef(1);
  const pendingInputLineRef = useRef("");
  const pendingInputReliableRef = useRef(true);
  const pendingSaveCountRef = useRef(0);
  const queuedInputsRef = useRef<string[]>([]);

  const registerAttachmentPlaceholders = useCallback((items: SessionAttachmentInfo[]) => {
    const slotByAttachmentId = placeholderSlotByAttachmentIdRef.current;
    const pathBySlot = attachmentPathByPlaceholderSlotRef.current;
    const ordered = [...items].sort((a, b) => a.createdAt - b.createdAt);
    for (const attachment of ordered) {
      let slot = slotByAttachmentId.get(attachment.id);
      if (slot === undefined) {
        slot = nextPlaceholderSlotRef.current;
        nextPlaceholderSlotRef.current += 1;
        slotByAttachmentId.set(attachment.id, slot);
      }
      pathBySlot.set(slot, attachment.path);
    }
  }, []);

  const resolveImagePlaceholders = useCallback((value: string): string => {
    return value.replace(IMAGE_PLACEHOLDER_PATTERN, (matched, slotText: string) => {
      const slot = Number.parseInt(slotText, 10);
      if (!Number.isFinite(slot)) {
        return matched;
      }
      return attachmentPathByPlaceholderSlotRef.current.get(slot) ?? matched;
    });
  }, []);

  const placeholderTokenForAttachment = useCallback((attachment: SessionAttachmentInfo): string | null => {
    const slot = placeholderSlotByAttachmentIdRef.current.get(attachment.id);
    if (slot === undefined) {
      return null;
    }
    return `[paste image ${slot}]`;
  }, []);

  useEffect(() => {
    let disposed = false;
    setAttachments([]);
    setAttachmentError(null);
    placeholderSlotByAttachmentIdRef.current.clear();
    attachmentPathByPlaceholderSlotRef.current.clear();
    nextPlaceholderSlotRef.current = 1;
    pendingInputLineRef.current = "";
    pendingInputReliableRef.current = true;
    listSessionAttachments(sessionId)
      .then((items) => {
        if (!disposed) {
          registerAttachmentPlaceholders(items);
          setAttachments(items);
        }
      })
      .catch((err) => {
        if (!disposed) {
          setAttachmentError(String(err));
        }
      });
    return () => {
      disposed = true;
    };
  }, [registerAttachmentPlaceholders, sessionId]);

  const saveImageFiles = useCallback(
    async (files: File[]): Promise<SessionAttachmentInfo[]> => {
      const imageFiles = files.filter(isImageFile);
      if (imageFiles.length === 0) {
        return [];
      }
      setIsSavingAttachment(true);
      setAttachmentError(null);
      try {
        const saved: SessionAttachmentInfo[] = [];
        for (const file of imageFiles) {
          const dataBase64 = await fileToBase64(file);
          saved.push(await saveSessionAttachment(sessionId, file.type || "image/png", dataBase64));
        }
        registerAttachmentPlaceholders(saved);
        setAttachments((current) => {
          const byId = new Map(current.map((attachment) => [attachment.id, attachment]));
          for (const attachment of saved) {
            byId.set(attachment.id, attachment);
          }
          return Array.from(byId.values()).sort((a, b) => a.createdAt - b.createdAt);
        });
        return saved;
      } catch (err) {
        setAttachmentError(String(err));
        return [];
      } finally {
        setIsSavingAttachment(false);
      }
    },
    [registerAttachmentPlaceholders, sessionId],
  );

  const handleDeleteAttachment = async (attachment: SessionAttachmentInfo) => {
    setAttachmentError(null);
    try {
      await deleteSessionAttachment(sessionId, attachment.path);
      const slot = placeholderSlotByAttachmentIdRef.current.get(attachment.id);
      if (slot !== undefined) {
        placeholderSlotByAttachmentIdRef.current.delete(attachment.id);
        attachmentPathByPlaceholderSlotRef.current.delete(slot);
      }
      setAttachments((current) => current.filter((item) => item.id !== attachment.id));
    } catch (err) {
      setAttachmentError(String(err));
    }
  };

  useEffect(() => {
    const shell = shellRef.current;
    const surface = surfaceRef.current;
    if (!shell || !surface) return;

    let disposed = false;
    let isConnecting = true;
    let unlistenPtyData: UnlistenFn | null = null;
    let unlistenSessionExited: UnlistenFn | null = null;
    let unlistenSessionError: UnlistenFn | null = null;
    let isReplaying = true;
    let isLive = false;
    let isActivating = false;
    let wasReplayOnly = false;
    let shouldClearReplayOnActivation = false;
    let pendingInput = "";
    const queuedLiveOutput: Array<string | Uint8Array> = [];

    setIsRestoring(false);
    setStatus("connecting");

    const terminal = new Terminal({
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily:
        '"Sarasa Term SC", "Maple Mono NF CN", "Maple Mono CN", "Noto Sans Mono CJK SC", "JetBrains Mono", "PingFang SC", "Microsoft YaHei", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
      fontSize: 13.5,
      lineHeight: 1.42,
      letterSpacing: 0,
      fontWeight: 430,
      fontWeightBold: 700,
      minimumContrastRatio: 5.2,
      drawBoldTextInBrightColors: false,
      convertEol: false,
      scrollback: 50000,
      theme: {
        background: "#101318",
        foreground: "#edf2f7",
        cursor: "#ff6f4c",
        selectionBackground: "rgba(255, 111, 76, 0.24)",
        black: "#0f1117",
        red: "#ff7875",
        green: "#7bd88f",
        yellow: "#f2c572",
        blue: "#85b7ff",
        magenta: "#d2a8ff",
        cyan: "#7ed1d8",
        white: "#edf2f7",
        brightBlack: "#7f8a9a",
        brightRed: "#f87171",
        brightGreen: "#86efac",
        brightYellow: "#f7d67c",
        brightBlue: "#9dc4ff",
        brightMagenta: "#d8b4fe",
        brightCyan: "#a7edf1",
        brightWhite: "#ffffff",
      },
    });
    const fitAddon = new FitAddon();

    terminal.loadAddon(fitAddon);
    terminal.open(surface);
    terminal.focus();
    let lastInterceptedKeydown: string | null = null;
    terminal.attachCustomKeyEventHandler((event) => {
      if (isConnecting) {
        event.preventDefault();
        event.stopPropagation();
        return false;
      }
      if (
        event.type === "keydown" &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.metaKey &&
        !event.isComposing &&
        (isLive ? isLiveDirectInterceptableInput(event.key) : isDirectInterceptablePrintable(event.key))
      ) {
        event.preventDefault();
        event.stopPropagation();
        lastInterceptedKeydown = event.key;
        pushInputRef.current?.(event.key);
        return false;
      }
      return true;
    });

    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;

    let lastWidth = 0;
    let lastHeight = 0;

    const refreshTerminal = () => {
      if (disposed) return;
      try {
        terminal.refresh(0, Math.max(0, terminal.rows - 1));
      } catch (err) {
        console.warn("Failed to refresh terminal:", err);
      }
    };

    const setCommandLinkMode = (enabled: boolean) => {
      if (commandLinkModeRef.current === enabled) {
        return;
      }
      commandLinkModeRef.current = enabled;
      shell.classList.toggle("terminal-link-mode", enabled);
      refreshTerminal();
    };

    const pathLinkDisposable = terminal.registerLinkProvider({
      provideLinks(bufferLineNumber, callback) {
        if (!onPreviewFile) {
          callback(undefined);
          return;
        }
        const line = terminal.buffer.active.getLine(bufferLineNumber - 1);
        if (!line) {
          callback(undefined);
          return;
        }

        const lineText = line.translateToString(true);
        const links = [];
        TERMINAL_FILE_PATH_PATTERN.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = TERMINAL_FILE_PATH_PATTERN.exec(lineText)) !== null) {
          const quotedWithDouble = Boolean(match[1]);
          const quotedWithSingle = Boolean(match[2]);
          const rawPath = match[1] ?? match[2] ?? match[3] ?? "";
          const targetPath = resolveTerminalPath(rawPath, cwd);
          if (!targetPath) {
            continue;
          }
          const shownPath = trimTerminalPathToken(rawPath);
          const startOffset = match.index + (quotedWithDouble || quotedWithSingle ? 1 : 0);
          const endOffset = startOffset + shownPath.length;
          if (endOffset <= startOffset) {
            continue;
          }
          links.push({
            range: {
              start: { x: startOffset + 1, y: bufferLineNumber },
              end: { x: endOffset, y: bufferLineNumber },
            },
            text: shownPath,
            decorations: {
              pointerCursor: true,
              underline: true,
            },
            activate(event: MouseEvent) {
              if (!event.metaKey) {
                return;
              }
              event.preventDefault();
              event.stopPropagation();
              onPreviewFile(targetPath);
            },
          });
        }
        callback(links.length > 0 ? links : undefined);
      },
    });

    const fitAndResize = (force = false) => {
      if (disposed) return;
      if (shell.clientWidth < 100 || shell.clientHeight < 50) {
        return;
      }

      const width = shell.clientWidth;
      const height = shell.clientHeight;
      if (!force && width === lastWidth && height === lastHeight) {
        return;
      }

      try {
        const dims = fitAddon.proposeDimensions();
        if (dims) {
          const rows = clampDimension(dims.rows, MIN_ROWS, MAX_ROWS);
          const cols = clampDimension(dims.cols - SCROLLBAR_GUTTER_COLS, MIN_COLS, MAX_COLS);
          if (rows !== terminal.rows || cols !== terminal.cols) {
            terminal.resize(cols, rows);
            resizeSession(sessionId, rows, cols).catch(() => undefined);
          }
        }
        lastWidth = width;
        lastHeight = height;
        refreshTerminal();
      } catch (err) {
        console.warn("Failed to fit terminal:", err);
      }
    };
    let resizeTimeout = 0;
    const debouncedFitAndResize = (force = false) => {
      window.clearTimeout(resizeTimeout);
      resizeTimeout = window.setTimeout(() => fitAndResize(force), 100);
    };
    const refreshAfterWindowRestore = () => {
      if (document.visibilityState === "hidden") {
        return;
      }
      window.requestAnimationFrame(() => {
        fitAndResize(true);
        refreshTerminal();
      });
      window.setTimeout(() => {
        fitAndResize(true);
        refreshTerminal();
      }, 120);
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        refreshAfterWindowRestore();
      }
    };
    const handleWindowResize = () => debouncedFitAndResize();
    const handleObservedResize: ResizeObserverCallback = () => debouncedFitAndResize();
    const handleDocumentKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Meta" || event.metaKey) {
        setCommandLinkMode(true);
      }
    };
    const handleDocumentKeyUp = (event: KeyboardEvent) => {
      if (event.key === "Meta" || !event.metaKey) {
        setCommandLinkMode(false);
      }
    };
    const handleWindowBlur = () => setCommandLinkMode(false);
    const handleShellMouseMove = (event: MouseEvent) => {
      if (event.metaKey) {
        setCommandLinkMode(true);
      }
    };
    const handleShellMouseLeave = () => {
      setCommandLinkMode(false);
    };
    const flushQueuedInputs = () => {
      const queue = [...queuedInputsRef.current];
      queuedInputsRef.current = [];
      for (const queuedData of queue) {
        const transformed = transformOutboundInput(queuedData);
        if (transformed) {
          if (!isLive) {
            if (isFocusOrMouseSequence(transformed)) {
              continue;
            }
            activateAndQueue(transformed);
          } else {
            writeSession(sessionId, transformed).catch(handleWriteFailure);
          }
        }
      }
    };

    const handlePaste = (event: ClipboardEvent) => {
      const files = clipboardImageFiles(event.clipboardData?.items);
      if (files.length === 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      
      pendingSaveCountRef.current += 1;
      void (async () => {
        try {
          const saved = await saveImageFiles(files);
          if (saved.length > 0) {
            const tokenText = saved
              .map((attachment) => placeholderTokenForAttachment(attachment))
              .filter((token): token is string => Boolean(token))
              .join(" ");
            if (tokenText) {
              const transformed = transformOutboundInput(`${tokenText} `);
              if (transformed) {
                if (!isLive) {
                  activateAndQueue(transformed);
                } else {
                  writeSession(sessionId, transformed).catch(handleWriteFailure);
                }
              }
            }
          }
        } finally {
          pendingSaveCountRef.current -= 1;
          if (pendingSaveCountRef.current === 0) {
            flushQueuedInputs();
          }
        }
      })();
    };
    const handleDragOver = (event: DragEvent) => {
      if (!dataTransferHasImage(event.dataTransfer?.items)) {
        return;
      }
      event.preventDefault();
      event.dataTransfer!.dropEffect = "copy";
    };
    const handleDrop = (event: DragEvent) => {
      const files = Array.from(event.dataTransfer?.files ?? []).filter(isImageFile);
      if (files.length === 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      
      pendingSaveCountRef.current += 1;
      void (async () => {
        try {
          const saved = await saveImageFiles(files);
          if (saved.length > 0) {
            const tokenText = saved
              .map((attachment) => placeholderTokenForAttachment(attachment))
              .filter((token): token is string => Boolean(token))
              .join(" ");
            if (tokenText) {
              const transformed = transformOutboundInput(`${tokenText} `);
              if (transformed) {
                if (!isLive) {
                  activateAndQueue(transformed);
                } else {
                  writeSession(sessionId, transformed).catch(handleWriteFailure);
                }
              }
            }
          }
        } finally {
          pendingSaveCountRef.current -= 1;
          if (pendingSaveCountRef.current === 0) {
            flushQueuedInputs();
          }
        }
      })();
    };
    const handleTerminalBeforeInput = (event: InputEvent) => {
      if (isConnecting) {
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      const data = event.data;
      const wasJustIntercepted = data !== null && lastInterceptedKeydown === data;
      lastInterceptedKeydown = null;
      if (
        event.inputType === "insertText" &&
        data !== null &&
        !event.isComposing &&
        (isLive ? isLiveDirectInterceptableInput(data) : isDirectInterceptablePrintable(data))
      ) {
        event.preventDefault();
        event.stopPropagation();
        if (!wasJustIntercepted) {
          pushInputRef.current?.(data);
        }
      }
    };

    window.addEventListener("resize", handleWindowResize);
    window.addEventListener("focus", refreshAfterWindowRestore);
    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("pageshow", refreshAfterWindowRestore);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("keydown", handleDocumentKeyDown, true);
    window.addEventListener("keyup", handleDocumentKeyUp, true);
    document.addEventListener("keydown", handleDocumentKeyDown, true);
    document.addEventListener("keyup", handleDocumentKeyUp, true);
    terminal.textarea?.addEventListener("beforeinput", handleTerminalBeforeInput);
    shell.addEventListener("mousemove", handleShellMouseMove, true);
    shell.addEventListener("mouseleave", handleShellMouseLeave);
    shell.addEventListener("paste", handlePaste, true);
    shell.addEventListener("dragover", handleDragOver);
    shell.addEventListener("drop", handleDrop);
    const resizeObserver = new ResizeObserver(handleObservedResize);
    resizeObserver.observe(shell);
    resizeObserver.observe(surface);

    const transformOutboundInput = (data: string): string => {
      if (isFocusOrMouseSequence(data) || data === BRACKETED_PASTE_START || data === BRACKETED_PASTE_END) {
        return data;
      }
      let output = "";
      let i = 0;
      while (i < data.length) {
        const char = data[i];

        if (char === "\x1b") {
          let seqLen = 1;
          if (i + 1 < data.length) {
            const next = data[i + 1];
            if (next === "[") {
              // CSI sequence
              seqLen = 2;
              while (i + seqLen < data.length) {
                const c = data[i + seqLen];
                const code = c.charCodeAt(0);
                seqLen++;
                if (code >= 0x40 && code <= 0x7E) {
                  break;
                }
              }
            } else if (next === "]") {
              // OSC sequence
              seqLen = 2;
              while (i + seqLen < data.length) {
                const c = data[i + seqLen];
                seqLen++;
                if (c === "\x07") {
                  break;
                }
                if (c === "\\" && data[i + seqLen - 2] === "\x1b") {
                  break;
                }
              }
            } else {
              seqLen = 2;
            }
          }

          const seq = data.slice(i, i + seqLen);
          output += seq;

          // If the sequence is a history navigation key (Up/Down arrow, PageUp/PageDown),
          // we mark the input line as unreliable.
          const isUpDown = seq === "\x1b[A" || seq === "\x1b[B";
          const isPageUpDown = seq.startsWith("\x1b[5~") || seq.startsWith("\x1b[6~");
          if (isUpDown || isPageUpDown) {
            pendingInputReliableRef.current = false;
          }

          i += seqLen;
          continue;
        }

        if (char === "\r" || char === "\n") {
          if (
            pendingInputReliableRef.current &&
            pendingInputLineRef.current.includes("[paste image")
          ) {
            const resolved = resolveImagePlaceholders(pendingInputLineRef.current);
            if (resolved !== pendingInputLineRef.current) {
              const backspaces = "\x7f".repeat(pendingInputLineRef.current.length);
              output += `${backspaces}${resolved}${char}`;
            } else {
              output += char;
            }
          } else {
            output += char;
          }
          pendingInputLineRef.current = "";
          pendingInputReliableRef.current = true;
          i++;
          continue;
        }

        if (!pendingInputReliableRef.current) {
          output += char;
          i++;
          continue;
        }

        if (char === "\x7f" || char === "\b") {
          pendingInputLineRef.current = pendingInputLineRef.current.slice(0, -1);
          output += char;
          i++;
          continue;
        }

        if (char === "\t") {
          pendingInputLineRef.current += char;
          output += char;
          i++;
          continue;
        }

        const code = char.charCodeAt(0);
        if (code >= 0x20 && code !== 0x7f) {
          pendingInputLineRef.current += char;
          output += char;
          i++;
          continue;
        }

        pendingInputReliableRef.current = false;
        output += char;
        i++;
      }
      return output;
    };

    const markSessionNotLive = (nextStatus: "readonly" | "error") => {
      if (disposed) return;
      isLive = false;
      isActivating = false;
      pendingInput = "";
      pendingInputLineRef.current = "";
      pendingInputReliableRef.current = true;
      queuedLiveOutput.splice(0);
      setStatus(nextStatus);
      setIsRestoring(false);
      refreshAfterWindowRestore();
    };

    const handleWriteFailure = (err: unknown) => {
      if (disposed) return;
      console.warn("Failed to write to PTY:", err);
      markSessionNotLive("readonly");
    };

    const writePtyPayload = (payload: string | Uint8Array) => {
      if (wasReplayOnly && isActivating && !isLive) {
        queuedLiveOutput.push(payload);
        return;
      }
      terminal.write(payload);
    };

    const switchReplayToLive = () => {
      if (!wasReplayOnly) {
        return;
      }
      if (shouldClearReplayOnActivation) {
        terminal.reset();
        terminal.clear();
        fitAndResize();
      }
      wasReplayOnly = false;
      queuedLiveOutput.splice(0).forEach((payload) => terminal.write(payload));
    };

    const flushPendingInput = () => {
      if (!pendingInput) {
        return;
      }
      const data = pendingInput;
      pendingInput = "";
      writeSession(sessionId, data).catch(handleWriteFailure);
    };

    const activateAndQueue = (data: string) => {
      pendingInput += data;
      if (isActivating) {
        return;
      }
      isActivating = true;
      setStatus("activating");
      setIsRestoring(true);
      reactivateSession(sessionId)
        .then((session) => {
          if (disposed) return;
          onSessionActivated?.(session);
          isLive = session.status === "running";
          isActivating = false;
          setStatus(isLive ? "attached" : "readonly");
          setIsRestoring(false);
          if (isLive) {
            switchReplayToLive();
            flushPendingInput();
          }
        })
        .catch((err) => {
          if (disposed) return;
          isActivating = false;
          pendingInput = "";
          setStatus("readonly");
          setIsRestoring(false);
          onActivationFailed?.(sessionId, String(err));
        });
    };

    activateAndQueueRef.current = activateAndQueue;
    pushInputRef.current = (data: string) => {
      if (pendingSaveCountRef.current > 0) {
        queuedInputsRef.current.push(data);
        return;
      }
      const transformed = transformOutboundInput(data);
      if (!transformed) {
        return;
      }
      if (!isLive) {
        if (isFocusOrMouseSequence(transformed)) {
          return;
        }
        activateAndQueue(transformed);
        return;
      }
      writeSession(sessionId, transformed).catch(handleWriteFailure);
    };

    const dataDisposable = terminal.onData((data) => {
      if (isReplaying) {
        return;
      }
      pushInputRef.current?.(data);
    });

    listen<SessionEvent>("session:exited", (event) => {
      if (event.payload.session.id === sessionId) {
        markSessionNotLive("readonly");
      }
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
        return;
      }
      unlistenSessionExited = unlisten;
    });

    listen<SessionErrorEvent>("session:error", (event) => {
      if (event.payload.sessionId === sessionId) {
        markSessionNotLive("error");
      }
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
        return;
      }
      unlistenSessionError = unlisten;
    });

    const waitForShellLayout = (): Promise<{ width: number; height: number }> => {
      return new Promise((resolve) => {
        if (!shell) {
          resolve({ width: 0, height: 0 });
          return;
        }
        if (shell.clientWidth >= 100 && shell.clientHeight >= 50) {
          resolve({ width: shell.clientWidth, height: shell.clientHeight });
          return;
        }
        const startTime = Date.now();
        const check = () => {
          if (disposed) {
            resolve({ width: 0, height: 0 });
            return;
          }
          const w = shell.clientWidth;
          const h = shell.clientHeight;
          if (w >= 100 && h >= 50) {
            resolve({ width: w, height: h });
          } else if (Date.now() - startTime > 1000) {
            resolve({ width: w, height: h });
          } else {
            requestAnimationFrame(check);
          }
        };
        requestAnimationFrame(check);
      });
    };

    async function connect() {
      try {
        const snapshot = await attachSession(sessionId);
        if (disposed) return;

        await waitForShellLayout();
        if (disposed) return;

        isLive = snapshot.mode === "live" && snapshot.session.status === "running";
        wasReplayOnly = !isLive;
        shouldClearReplayOnActivation = snapshot.session.agentId === "claude-code";
        fitAndResize(true);
        const onWriteComplete = () => {
          isReplaying = false;
          isConnecting = false;
          if (!disposed) {
            setIsRestoring(false);
          }
        };
        const replayBytes = snapshot.replayBase64 ? decodeBase64Bytes(snapshot.replayBase64) : null;
        if (replayBytes && replayBytes.length > 0) {
          terminal.write(replayBytes, onWriteComplete);
        } else if (snapshot.replay && snapshot.replay.length > 0) {
          terminal.write(snapshot.replay, onWriteComplete);
        } else {
          onWriteComplete();
        }
        unlistenPtyData = await listen<PtyDataEvent>("pty:data", (event) => {
          if (event.payload.sessionId === sessionId) {
            if (event.payload.dataBase64) {
              const bytes = decodeBase64Bytes(event.payload.dataBase64);
              if (bytes) {
                writePtyPayload(bytes);
              }
            } else if (event.payload.data) {
              writePtyPayload(event.payload.data);
            }
          }
        });
        setStatus(isLive ? "attached" : "readonly");
        setTimeout(() => {
          if (!disposed) {
            terminal.focus();
          }
        }, 150);
      } catch (err) {
        setStatus("error");
        setIsRestoring(false);
        isConnecting = false;
        terminal.writeln(`[waypoint attach error] ${String(err)}`);
      }
    }

    connect();

    return () => {
      disposed = true;
      setIsRestoring(false);
      activateAndQueueRef.current = null;
      pushInputRef.current = null;
      window.clearTimeout(resizeTimeout);
      detachSession(sessionId).catch(() => undefined);
      dataDisposable.dispose();
      pathLinkDisposable.dispose();
      window.removeEventListener("resize", handleWindowResize);
      window.removeEventListener("focus", refreshAfterWindowRestore);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("pageshow", refreshAfterWindowRestore);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("keydown", handleDocumentKeyDown, true);
      window.removeEventListener("keyup", handleDocumentKeyUp, true);
      document.removeEventListener("keydown", handleDocumentKeyDown, true);
      document.removeEventListener("keyup", handleDocumentKeyUp, true);
      terminal.textarea?.removeEventListener("beforeinput", handleTerminalBeforeInput);
      shell.removeEventListener("mousemove", handleShellMouseMove, true);
      shell.removeEventListener("mouseleave", handleShellMouseLeave);
      shell.removeEventListener("paste", handlePaste, true);
      shell.removeEventListener("dragover", handleDragOver);
      shell.removeEventListener("drop", handleDrop);
      resizeObserver.disconnect();
      unlistenPtyData?.();
      unlistenSessionExited?.();
      unlistenSessionError?.();
      terminal.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
      pendingInputLineRef.current = "";
      pendingInputReliableRef.current = true;
    };
  }, [cwd, onPreviewFile, placeholderTokenForAttachment, resolveImagePlaceholders, sessionId, saveImageFiles]);

  const handleContainerClick = () => {
    terminalRef.current?.focus();
  };

  return (
    <div className="terminal-view" data-status={status}>
      {status === "readonly" && !isRestoring && (
        <div className="resume-banner">
          <span className="banner-icon" aria-hidden="true">i</span>
          <span>
            当前为历史只读会话。
            <button
              type="button"
              className="resume-link-btn"
              onClick={(e) => {
                e.stopPropagation();
                activateAndQueueRef.current?.("");
              }}
            >
              点击此处
            </button>
            或在终端输入任意内容以恢复会话。
          </span>
        </div>
      )}
      <div className="terminal-workbench">
        <div className="terminal-shell" data-status={status} onClick={handleContainerClick} ref={shellRef}>
          <div className="terminal-surface" ref={surfaceRef} />
          {status === "connecting" ? (
            <div className="terminal-restore-overlay" role="status" aria-live="polite">
              <div className="terminal-restore-panel">
                <span className="terminal-restore-spinner" aria-hidden="true" />
                <div>
                  <strong>正在加载会话</strong>
                  <span>正在加载终端内容，请稍候...</span>
                </div>
              </div>
            </div>
          ) : null}
          {isRestoring && status !== "connecting" ? (
            <div className="terminal-restore-overlay" role="status" aria-live="polite">
              <div className="terminal-restore-panel">
                <span className="terminal-restore-spinner" aria-hidden="true" />
                <div>
                  <strong>正在恢复会话</strong>
                  <span>正在连接 Agent 原生历史，恢复完成后会继续显示会话内容。</span>
                </div>
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

export default TerminalView;
