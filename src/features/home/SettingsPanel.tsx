import { useEffect, useRef, useState } from "react";
import {
  Keyboard,
  FolderOpen,
  Save,
  Rocket,
  History,
  RotateCcw,
  Shirt,
  RefreshCw,
  Download,
} from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import {
  getSettings,
  updateSettings,
  resetSettings,
  pickFolder,
  hotkeysSuspend,
  hotkeysResume,
  openLogDir,
  type Settings,
} from "@/lib/ipc";
import {
  cachedUpdate,
  checkUpdate,
  installUpdate,
  type UpdateInfo,
  type UpdateProgress,
} from "@/lib/updater";
import { cn } from "@/lib/cn";

export default function SettingsPanel({
  focusUpdate = false,
  checkNonce = 0,
}: {
  focusUpdate?: boolean;
  checkNonce?: number;
}) {
  const [s, setS] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [resetArmed, setResetArmed] = useState(false);
  const updateRef = useRef<HTMLDivElement>(null);
  const saveTimer = useRef<number | null>(null);
  const resetTimer = useRef<number | null>(null);

  useEffect(() => {
    getSettings().then(setS);
  }, []);

  // 从首页「新版本」/ 托盘「检查更新」进来时，直接滚到更新区并闪一下边框
  useEffect(() => {
    if (!focusUpdate) return;
    const scroll = window.setTimeout(
      () => updateRef.current?.scrollIntoView({ behavior: "smooth", block: "center" }),
      80,
    );
    return () => window.clearTimeout(scroll);
  }, [focusUpdate]);

  useEffect(
    () => () => {
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
      if (resetTimer.current) window.clearTimeout(resetTimer.current);
    },
    [],
  );

  const onReset = async () => {
    if (!resetArmed) {
      setResetArmed(true);
      if (resetTimer.current) window.clearTimeout(resetTimer.current);
      resetTimer.current = window.setTimeout(() => setResetArmed(false), 2500);
      return;
    }
    setResetArmed(false);
    try {
      const next = await resetSettings();
      setS(next);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  /** 更新并防抖持久化；快捷键注册失败时回显错误并还原。 */
  const patch = (p: Partial<Settings>) => {
    setS((prev) => {
      if (!prev) return prev;
      const next = { ...prev, ...p };
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(async () => {
        try {
          await updateSettings(next);
          setError(null);
        } catch (err) {
          setError(String(err));
          getSettings().then(setS);
        }
      }, 350);
      return next;
    });
  };

  if (!s) {
    return <div className="grid h-full place-items-center text-[12px] text-white/25">加载中…</div>;
  }

  return (
    <div className="h-full overflow-y-auto px-9 pt-4 pb-8">
      <h2 className="display text-[17px] text-white/88">设置</h2>
      <p className="mt-1 text-[11px] tracking-[0.01em] text-white/28">所有更改即时生效并自动保存</p>

      {error && (
        <div className="mt-3 flex items-center gap-2 rounded-[10px] border border-white/[0.08] bg-white/[0.03] px-3 py-2 text-[11.5px] text-white/70">
          <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-red-400/80" />
          {error}
        </div>
      )}

      <Section icon={Keyboard} title="快捷键">
        <Row label="截图" hint="全局生效，与其他软件冲突时会提示">
          <HotkeyInput value={s.hotkeyCapture} onChange={(v) => patch({ hotkeyCapture: v })} />
        </Row>
        <Row label="全屏即拍" hint="瞬间截取光标所在屏幕并复制（「直接保存」模式下同时存盘）">
          <HotkeyInput value={s.hotkeyFull} onChange={(v) => patch({ hotkeyFull: v })} />
        </Row>
        <Row label="窗口即拍" hint="瞬间截取当前活动窗口并复制，完成后有通知">
          <HotkeyInput value={s.hotkeyWindow} onChange={(v) => patch({ hotkeyWindow: v })} />
        </Row>
        <Row label="重复上次区域" hint="按上次的选区重新截图，可再调整">
          <HotkeyInput value={s.hotkeyRepeat} onChange={(v) => patch({ hotkeyRepeat: v })} />
        </Row>
        <Row label="贴图剪贴板图片" hint="把剪贴板里的图片直接钉到屏幕上">
          <HotkeyInput value={s.hotkeyPinClip} onChange={(v) => patch({ hotkeyPinClip: v })} />
        </Row>
        <Row label="隐藏 / 显示全部贴图" hint="一键收起或恢复所有置顶贴图">
          <HotkeyInput
            value={s.hotkeyTogglePins}
            onChange={(v) => patch({ hotkeyTogglePins: v })}
          />
        </Row>
      </Section>

      <Section icon={Save} title="保存">
        <Row label="保存目录" hint="留空 = 图片库\Snapix">
          <div className="flex w-72 items-center gap-1.5">
            <input
              readOnly
              value={s.saveDir}
              placeholder="图片库\Snapix（默认）"
              className="h-8 min-w-0 flex-1 rounded-[9px] border border-white/[0.07] bg-white/[0.03] px-2.5 text-[11.5px] text-white/75 outline-none placeholder:text-white/20"
            />
            {s.saveDir && (
              <button
                title="恢复默认"
                onClick={() => patch({ saveDir: "" })}
                className="grid h-8 w-8 shrink-0 place-items-center rounded-[9px] text-white/30 transition-colors hover:bg-white/[0.06] hover:text-white/80"
              >
                <RotateCcw size={13} />
              </button>
            )}
            <button
              onClick={async () => {
                try {
                  const dir = await pickFolder();
                  if (dir) patch({ saveDir: dir });
                } catch (err) {
                  setError(String(err));
                }
              }}
              className="grid h-8 w-8 shrink-0 place-items-center rounded-[9px] border border-white/[0.08] bg-white/[0.04] text-white/60 transition-colors hover:bg-white/[0.08] hover:text-white/90"
            >
              <FolderOpen size={14} />
            </button>
          </div>
        </Row>
        <Row label="文件命名" hint="支持 {date} {time} {w} {h}">
          <input
            value={s.fileTemplate}
            onChange={(e) => patch({ fileTemplate: e.target.value })}
            spellCheck={false}
            className="h-8 w-72 rounded-[9px] border border-white/[0.07] bg-white/[0.03] px-2.5 text-[11.5px] text-white/75 outline-none transition-colors focus:border-white/20 focus:bg-white/[0.05]"
          />
        </Row>
        <Row label="图片格式">
          <Segmented
            value={s.imageFormat}
            options={[
              { v: "png", label: "PNG" },
              { v: "jpg", label: "JPG" },
            ]}
            onChange={(v) => patch({ imageFormat: v as Settings["imageFormat"] })}
          />
        </Row>
        <Row label="保存方式">
          <Segmented
            value={s.saveAsk ? "ask" : "direct"}
            options={[
              { v: "ask", label: "每次询问" },
              { v: "direct", label: "直接保存" },
            ]}
            onChange={(v) => patch({ saveAsk: v === "ask" })}
          />
        </Row>
        <Row label="保存后复制到剪贴板">
          <Toggle checked={s.copyAfterSave} onChange={(v) => patch({ copyAfterSave: v })} />
        </Row>
        <Row label="截图包含鼠标光标" hint="把当前光标画进截图；全屏 / 窗口即拍同样生效">
          <Toggle checked={s.captureCursor} onChange={(v) => patch({ captureCursor: v })} />
        </Row>
      </Section>

      <Section icon={History} title="历史记录">
        <Row label="启用历史记录" hint="截图与标注产出自动留存本地">
          <Toggle checked={s.historyEnabled} onChange={(v) => patch({ historyEnabled: v })} />
        </Row>
        <Row label="保留上限">
          <Segmented
            value={String(s.historyLimit)}
            options={[
              { v: "50", label: "50" },
              { v: "100", label: "100" },
              { v: "200", label: "200" },
              { v: "500", label: "500" },
            ]}
            onChange={(v) => patch({ historyLimit: Number(v) })}
          />
        </Row>
      </Section>

      <Section icon={Shirt} title="晾衣绳">
        <Row
          label="截图后自动挂到晾衣绳"
          hint="产出图挂到屏幕顶部，单击截图平滑展开；托盘可随时显示 / 隐藏"
        >
          <Toggle checked={s.lineEnabled} onChange={(v) => patch({ lineEnabled: v })} />
        </Row>
      </Section>

      <Section icon={Rocket} title="通用">
        <Row label="开机自启" hint="登录 Windows 后静默启动到托盘">
          <Toggle checked={s.autostart} onChange={(v) => patch({ autostart: v })} />
        </Row>
        <Row label="打开日志目录" hint="反馈问题时可附上 snapix.log，便于定位">
          <button
            onClick={() => openLogDir().catch((e) => setError(String(e)))}
            className="inline-flex h-7 items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.05] px-2.5 text-[11px] text-white/70 transition-colors hover:bg-white/[0.09] hover:text-white"
          >
            <FolderOpen size={11} />
            打开
          </button>
        </Row>
        <Row label="恢复默认设置" hint="快捷键、保存、历史、晾衣绳与更新开关全部回到初始值">
          <button
            onClick={onReset}
            className={cn(
              "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-[11px] transition-colors",
              resetArmed
                ? "border-red-400/40 bg-red-400/15 text-red-200"
                : "border-white/10 bg-white/[0.05] text-white/70 hover:bg-white/[0.09] hover:text-white",
            )}
          >
            <RotateCcw size={11} />
            {resetArmed ? "确认恢复?" : "恢复默认"}
          </button>
        </Row>
      </Section>

      <div
        ref={updateRef}
        className={cn(
          "rounded-[16px] transition-shadow duration-300",
          focusUpdate && "ring-2 ring-white/15",
        )}
      >
        <UpdateSection
          updateCheck={s.updateCheck}
          onUpdateCheck={(v) => patch({ updateCheck: v })}
          checkNonce={checkNonce}
        />
      </div>
    </div>
  );
}

/** 关于与更新：版本号 + 手动检查 + 下载安装（Windows 安装器会接管重启）。 */
function UpdateSection({
  updateCheck,
  onUpdateCheck,
  checkNonce,
}: {
  updateCheck: boolean;
  onUpdateCheck: (v: boolean) => void;
  checkNonce: number;
}) {
  const [version, setVersion] = useState("");
  const [update, setUpdate] = useState<UpdateInfo | null>(() => cachedUpdate());
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [progress, setProgress] = useState<UpdateProgress>({ downloaded: 0, total: null });
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  const onCheck = async () => {
    if (checking || installing) return;
    setChecking(true);
    setMessage(null);
    try {
      const info = await checkUpdate();
      setUpdate(info);
      setMessage(info ? null : "当前已是最新版本");
    } catch (e) {
      setMessage(`检查失败：${String(e)}`);
    } finally {
      setChecking(false);
    }
  };

  const onInstall = async () => {
    if (!update || installing) return;
    setInstalling(true);
    setMessage(null);
    setProgress({ downloaded: 0, total: null });
    try {
      await installUpdate(setProgress);
      setMessage("安装程序已启动，应用将自动重启");
    } catch (e) {
      setMessage(`更新失败：${String(e)}`);
    } finally {
      setInstalling(false);
    }
  };

  // 托盘「检查更新」：nonce 变化时自动跑一次
  const lastNonce = useRef(0);
  useEffect(() => {
    if (checkNonce > 0 && checkNonce !== lastNonce.current) {
      lastNonce.current = checkNonce;
      void onCheck();
    }
    // onCheck 每次渲染都是新函数，这里只认 nonce 的变化
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [checkNonce]);

  const percent =
    progress.total && progress.total > 0
      ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
      : null;

  return (
    <Section icon={RefreshCw} title="关于与更新">
      <Row label="启动时自动检查" hint="每 6 小时最多检查一次；托盘「检查更新」始终可用">
        <Toggle checked={updateCheck} onChange={onUpdateCheck} />
      </Row>
      <Row label="当前版本" hint="通过 GitHub Releases 获取更新，下载后校验签名再安装">
        <div className="flex items-center gap-2.5">
          <span className="text-[11.5px] tabular-nums text-white/40">
            {version ? `v${version}` : "—"}
          </span>
          <button
            onClick={onCheck}
            disabled={checking || installing}
            className="inline-flex h-7 items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.05] px-2.5 text-[11px] text-white/70 transition-colors hover:bg-white/[0.09] hover:text-white disabled:opacity-40"
          >
            <RefreshCw size={11} className={checking ? "animate-spin" : undefined} />
            {checking ? "检查中" : "检查更新"}
          </button>
        </div>
      </Row>

      {(update || message || installing) && (
        <div className="py-3">
          {update && (
            <>
              <div className="flex items-center gap-1.5 text-[12px] text-white/80">
                <Download size={12} strokeWidth={1.8} className="text-white/40" />
                发现新版本 v{update.version}
              </div>
              {update.notes && (
                <p className="mt-1.5 max-h-28 overflow-y-auto whitespace-pre-wrap text-[10.5px] leading-relaxed text-white/32">
                  {plainNotes(update.notes)}
                </p>
              )}
              <div className="mt-2.5 flex items-center gap-2.5">
                <button
                  onClick={onInstall}
                  disabled={installing}
                  className="inline-flex h-7 items-center gap-1.5 rounded-full bg-white px-3 text-[11px] font-medium text-black transition-colors hover:bg-white/85 disabled:opacity-50"
                >
                  <Download size={11} strokeWidth={2} />
                  {installing ? "正在下载…" : "下载并安装"}
                </button>
                {installing && percent !== null && (
                  <span className="text-[10.5px] tabular-nums text-white/35">{percent}%</span>
                )}
              </div>
              {installing && (
                <div className="mt-2 h-[3px] overflow-hidden rounded-full bg-white/10">
                  <div
                    className="h-full rounded-full bg-white/70 transition-[width] duration-200"
                    style={{ width: `${percent ?? 12}%` }}
                  />
                </div>
              )}
            </>
          )}
          {message && (
            <div className={cn("text-[10.5px] leading-relaxed", update && "mt-2")}>
              <span className={message.includes("失败") ? "text-red-300/70" : "text-white/32"}>
                {message}
              </span>
            </div>
          )}
        </div>
      )}
    </Section>
  );
}

/* ===================== 小组件 ===================== */

/** Release 的 notes 是 Markdown，这里只做轻量去标记，避免把反引号/星号直接露给用户。 */
function plainNotes(text: string) {
  return text
    .replace(/```[\s\S]*?```/g, (m) => m.replace(/```/g, ""))
    .replace(/[*_`>#]/g, "")
    .replace(/\[(.*?)\]\(.*?\)/g, "$1")
    .trim();
}

function Section({
  icon: Icon,
  title,
  children,
}: {
  icon: typeof Keyboard;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-6">
      <div className="mb-2 flex items-center gap-1.5 pl-1 text-[11px] font-medium tracking-[0.04em] text-white/38">
        <Icon size={12} strokeWidth={1.7} />
        {title}
      </div>
      <div className="divide-y divide-white/[0.045] rounded-[14px] border border-white/[0.06] bg-white/[0.022] px-4">
        {children}
      </div>
    </section>
  );
}

function Row({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-[50px] items-center justify-between gap-4 py-2.5">
      <div>
        <div className="text-[12.5px] text-white/78">{label}</div>
        {hint && <div className="mt-1 text-[10.5px] leading-relaxed text-white/30">{hint}</div>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

/** 单色开关：开 = 白色轨道 + 深色滑块，关 = 浅灰轨道 + 白色滑块。 */
function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative rounded-full transition-colors duration-200",
        checked ? "bg-white" : "bg-white/[0.13]",
      )}
      style={{ height: 22, width: 38 }}
    >
      <span
        className={cn(
          "absolute top-[3px] h-4 w-4 rounded-full transition-all duration-200",
          checked ? "bg-[#0a0a0c]" : "bg-white/80",
        )}
        style={{ left: checked ? 20 : 3 }}
      />
    </button>
  );
}

function Segmented({
  value,
  options,
  onChange,
}: {
  value: string;
  options: { v: string; label: string }[];
  onChange: (v: string) => void;
}) {
  return (
    <div className="flex items-center gap-0.5 rounded-full border border-white/[0.06] bg-white/[0.035] p-0.5">
      {options.map((o) => (
        <button
          key={o.v}
          onClick={() => onChange(o.v)}
          className={cn(
            "rounded-full px-3 py-[5px] text-[11.5px] transition-colors duration-150",
            value === o.v
              ? "bg-white/[0.13] text-white shadow-[inset_0_1px_0_rgba(255,255,255,0.08)]"
              : "text-white/40 hover:text-white/75",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** 可以单独作为全局快捷键的键：日常输入用不到，不会抢占其它软件。 */
const STANDALONE_KEYS = new Set([
  "PrintScreen",
  "ScrollLock",
  "Pause",
  "Insert",
  ...Array.from({ length: 24 }, (_, i) => `F${i + 1}`),
]);

/** 其它可用键（必须配合修饰键）：W3C code → 快捷键写法（Rust 侧解析器接受的形式）。 */
const COMBO_KEYS: Record<string, string> = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  NumpadAdd: "NumpadAdd",
  NumpadSubtract: "NumpadSubtract",
  NumpadMultiply: "NumpadMultiply",
  NumpadDivide: "NumpadDivide",
  NumpadDecimal: "NumpadDecimal",
  NumpadEnter: "NumpadEnter",
  ...Object.fromEntries(Array.from({ length: 10 }, (_, i) => [`Numpad${i}`, `Numpad${i}`])),
};

/** 键盘事件的 code → 快捷键中的键名；不支持的键返回 null。 */
function hotkeyKeyFromCode(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (STANDALONE_KEYS.has(code)) return code;
  return COMBO_KEYS[code] ?? null;
}

const MODIFIER_CODE = /^(Control|Alt|Shift|Meta)(Left|Right)$/;

/**
 * 快捷键录制：点击后按下想要的键。F1–F24、PrintScreen、Insert 等可以单独使用，
 * 字母 / 数字 / 符号等需配合 Ctrl、Alt、Shift；Esc 取消。
 */
function HotkeyInput({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const [recording, setRecording] = useState(false);
  const [hint, setHint] = useState<string | null>(null);
  // 父组件每次渲染都会传入新的 onChange；用 ref 持有，避免录制中的 effect 反复
  // 重跑（每次重跑都会 resume 再 suspend 一遍全局热键）
  const onChangeRef = useRef(onChange);
  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  const toggleRecording = () => {
    setHint(null);
    setRecording((r) => !r);
  };

  useEffect(() => {
    if (!recording) return;
    // 录制期间暂停全局热键，否则按下当前截图键会直接触发截图
    hotkeysSuspend();
    const onKey = (e: KeyboardEvent) => {
      // Windows 会截获 PrintScreen 的 keydown，网页只能收到 keyup；其余键都在 keydown 处理
      const isPrint = e.code === "PrintScreen";
      if ((e.type === "keyup") !== isPrint) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setHint(null);
        setRecording(false);
        return;
      }

      const mods: string[] = [];
      if (e.ctrlKey) mods.push("Ctrl");
      if (e.altKey) mods.push("Alt");
      if (e.shiftKey) mods.push("Shift");
      if (e.metaKey) mods.push("Super");

      const key = hotkeyKeyFromCode(e.code);
      if (!key) {
        // 只按了修饰键：继续等主键；其它键：明确告知不支持，而不是没反应
        if (!MODIFIER_CODE.test(e.code)) setHint("不支持这个按键，换一个试试");
        return;
      }
      if (mods.length === 0 && !STANDALONE_KEYS.has(e.code)) {
        setHint("这个键需要配合 Ctrl、Alt 或 Shift；F1–F24、PrintScreen、Insert 可单独使用");
        return;
      }

      setHint(null);
      setRecording(false);
      onChangeRef.current([...mods, key].join("+"));
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
      hotkeysResume();
    };
  }, [recording]);

  return (
    <div className="flex flex-col items-end gap-1">
      <div className="flex items-center gap-1.5">
        {value && !recording && (
          <button
            title="清除快捷键"
            onClick={() => onChange("")}
            className="grid h-8 w-8 place-items-center rounded-[9px] text-white/25 transition-colors hover:bg-white/[0.06] hover:text-white/70"
          >
            <RotateCcw size={12} />
          </button>
        )}
        <button
          onClick={toggleRecording}
          className={cn(
            "h-8 min-w-40 rounded-[9px] border px-3 text-[11.5px] font-medium tabular-nums transition-colors duration-200",
            recording
              ? "border-white/25 bg-white/[0.12] text-white"
              : "border-white/[0.08] bg-white/[0.04] text-white/70 hover:bg-white/[0.08] hover:text-white/90",
          )}
        >
          {recording ? "按下新快捷键… (Esc 取消)" : value || "点击设置"}
        </button>
      </div>
      {recording && (
        <div
          className={cn(
            "max-w-64 text-right text-[10.5px] leading-relaxed",
            hint ? "text-white/60" : "text-white/25",
          )}
        >
          {hint ?? "可单键（如 F1、PrintScreen），或 Ctrl / Alt / Shift 组合"}
        </div>
      )}
      {!recording && value.includes("PrintScreen") && (
        <div className="max-w-64 text-right text-[10.5px] leading-relaxed text-white/30">
          若同时弹出 Windows 截图工具，请在「设置 → 辅助功能 → 键盘」关闭「使用 Print Screen
          打开屏幕截图」
        </div>
      )}
    </div>
  );
}
