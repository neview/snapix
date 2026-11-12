import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { motion } from "motion/react";
import {
  Copy,
  Pin,
  PenLine,
  Save,
  Trash2,
  FolderOpen,
  ImageOff,
  Eraser,
  AlertCircle,
} from "lucide-react";
import {
  historyList,
  historyDelete,
  historyClear,
  historyOpenDir,
  historyCopy,
  historySaveAs,
  historyPin,
  historyEdit,
  type HistoryEntry,
} from "@/lib/ipc";
import { cn } from "@/lib/cn";

function fmtTime(ts: number) {
  const d = new Date(ts);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  if (sameDay) return `今天 ${hm}`;
  return `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

export default function HistoryPanel() {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [confirmClear, setConfirmClear] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const errorTimer = useRef<number | null>(null);
  // 正在处理中的条目：同一张图的按钮在请求返回前禁用，避免连点弹出两个原生保存框、
  // 或重复解码整图（每张 4K 图解码约 30MB）。
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  // 删除需二次确认：列表会因删除重排，下一张图顶到光标下，误连点会连续删除。
  const [confirmDel, setConfirmDel] = useState<string | null>(null);
  const confirmTimer = useRef<number | null>(null);

  const refresh = useCallback(() => {
    historyList()
      .then(setEntries)
      .catch(() => {});
  }, []);

  /** 把后端返回的错误显示几秒，避免操作静默失败。 */
  const showError = useCallback((e: unknown) => {
    setError(String(e));
    if (errorTimer.current) window.clearTimeout(errorTimer.current);
    errorTimer.current = window.setTimeout(() => setError(null), 3000);
  }, []);

  /** 带在途标记执行单个条目的操作：同一 id 未返回前按钮不可再点。 */
  const runItem = useCallback(
    (id: string, p: () => Promise<unknown>) => {
      setBusy((b) => (b.has(id) ? b : new Set(b).add(id)));
      p()
        .catch(showError)
        .finally(() =>
          setBusy((b) => {
            const next = new Set(b);
            next.delete(id);
            return next;
          }),
        );
    },
    [showError],
  );

  /** 清空：两段式确认（2500ms 内再点一次才执行）。 */
  const runClear = useCallback(() => {
    historyClear()
      .catch(showError)
      .finally(() => setConfirmClear(false));
  }, [showError]);

  useEffect(() => {
    refresh();
    const un = listen("history:changed", refresh);
    return () => {
      un.then((f) => f());
    };
  }, [refresh]);

  useEffect(
    () => () => {
      if (errorTimer.current) window.clearTimeout(errorTimer.current);
      if (confirmTimer.current) window.clearTimeout(confirmTimer.current);
    },
    [],
  );

  /** 删除按钮：首次点击进入确认态，再次点击才真正删除。 */
  const askDelete = (id: string) => {
    if (confirmDel === id) {
      setConfirmDel(null);
      runItem(id, () => historyDelete(id));
      return;
    }
    setConfirmDel(id);
    if (confirmTimer.current) window.clearTimeout(confirmTimer.current);
    confirmTimer.current = window.setTimeout(() => setConfirmDel(null), 2500);
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-end justify-between px-9 pt-4 pb-4">
        <div>
          <h2 className="display text-[17px] text-white/88">历史记录</h2>
          <p className="mt-1 text-[11px] tracking-[0.01em] text-white/28">
            共 {entries.length} 张 · 截图与标注产出自动留存
          </p>
        </div>
        <div className="flex items-center gap-1">
          {error && (
            <span className="mr-1 flex items-center gap-1 text-[11px] text-red-300/85">
              <AlertCircle size={12} />
              {error}
            </span>
          )}
          <button
            onClick={() => historyOpenDir().catch(showError)}
            className="flex items-center gap-1.5 rounded-full px-2.5 py-1.5 text-[11.5px] text-white/40 transition-colors duration-200 hover:bg-white/[0.06] hover:text-white/80"
          >
            <FolderOpen size={13} strokeWidth={1.7} />
            打开文件夹
          </button>
          <button
            onClick={() => {
              if (confirmClear) {
                runClear();
              } else {
                setConfirmClear(true);
                setTimeout(() => setConfirmClear(false), 2500);
              }
            }}
            disabled={!entries.length}
            className={cn(
              "flex items-center gap-1.5 rounded-full px-2.5 py-1.5 text-[11.5px] transition-colors duration-200 disabled:opacity-30",
              confirmClear
                ? "bg-white text-[#0a0a0c]"
                : "text-white/40 hover:bg-white/[0.06] hover:text-white/80",
            )}
          >
            <Eraser size={13} strokeWidth={1.7} />
            {confirmClear ? "再点一次清空" : "清空"}
          </button>
        </div>
      </div>

      {entries.length === 0 ? (
        <div className="grid flex-1 place-items-center">
          <div className="flex flex-col items-center gap-3 text-white/20">
            <ImageOff size={30} strokeWidth={1.3} />
            <p className="text-[12px]">还没有截图记录，按下快捷键截一张吧</p>
          </div>
        </div>
      ) : (
        <div className="grid flex-1 auto-rows-min grid-cols-2 gap-2.5 overflow-y-auto px-9 pb-6 sm:grid-cols-3 lg:grid-cols-4">
          {entries.map((e, i) => (
            <motion.div
              key={e.id}
              initial={{ opacity: 0, y: 6 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.3, delay: Math.min(i * 0.015, 0.24) }}
              className="group relative overflow-hidden rounded-[12px] border border-white/[0.06] bg-white/[0.022] transition-colors duration-200 hover:border-white/[0.13]"
            >
              <div
                className="grid h-[118px] place-items-center overflow-hidden"
                style={{
                  background:
                    "repeating-conic-gradient(rgba(255,255,255,.028) 0% 25%, transparent 0% 50%) 50% / 12px 12px",
                }}
              >
                <img
                  src={e.url}
                  alt=""
                  loading="lazy"
                  draggable={false}
                  className="max-h-full max-w-full object-contain"
                />
              </div>

              <div className="flex items-center justify-between px-2.5 py-2 text-[10.5px] tabular-nums text-white/28">
                <span>{fmtTime(e.ts)}</span>
                <span>
                  {e.width}×{e.height}
                  {e.source === "editor" && <span className="ml-1 text-white/45">· 标注</span>}
                </span>
              </div>

              {/* 悬停操作层 */}
              <div className="absolute inset-x-0 top-0 flex h-[118px] items-center justify-center gap-1 bg-black/45 opacity-0 backdrop-blur-[3px] transition-opacity duration-200 group-hover:opacity-100">
                <MiniBtn
                  title="复制"
                  disabled={busy.has(e.id)}
                  onClick={() => runItem(e.id, () => historyCopy(e.id))}
                >
                  <Copy size={14} />
                </MiniBtn>
                <MiniBtn
                  title="贴图置顶"
                  disabled={busy.has(e.id)}
                  onClick={() => runItem(e.id, () => historyPin(e.id))}
                >
                  <Pin size={14} />
                </MiniBtn>
                <MiniBtn
                  title="再编辑"
                  disabled={busy.has(e.id)}
                  onClick={() => runItem(e.id, () => historyEdit(e.id))}
                >
                  <PenLine size={14} />
                </MiniBtn>
                <MiniBtn
                  title="另存为"
                  disabled={busy.has(e.id)}
                  onClick={() => runItem(e.id, () => historySaveAs(e.id))}
                >
                  <Save size={14} />
                </MiniBtn>
                <MiniBtn
                  title={confirmDel === e.id ? "再点一次确认删除" : "删除"}
                  disabled={busy.has(e.id)}
                  onClick={() => askDelete(e.id)}
                  className={confirmDel === e.id ? "bg-white text-[#0a0a0c]" : undefined}
                >
                  {confirmDel === e.id ? (
                    <span className="text-[9.5px] font-semibold">确认</span>
                  ) : (
                    <Trash2 size={13} />
                  )}
                </MiniBtn>
              </div>
            </motion.div>
          ))}
        </div>
      )}
    </div>
  );
}

function MiniBtn({
  children,
  title,
  onClick,
  disabled,
  className,
}: {
  children: React.ReactNode;
  title: string;
  onClick: () => void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "grid h-7 w-7 place-items-center rounded-full border border-white/[0.08] bg-white/[0.05] text-white/65 transition-all duration-150 disabled:cursor-not-allowed disabled:opacity-40",
        "hover:border-white/[0.2] hover:bg-white/15 hover:text-white",
        className,
      )}
    >
      {children}
    </button>
  );
}
