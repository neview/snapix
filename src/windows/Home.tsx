import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { motion } from "motion/react";
import {
  Camera,
  ScanText,
  Pin,
  ScrollText,
  Sparkles,
  History,
  Settings as SettingsIcon,
  Keyboard,
  Timer,
  RotateCw,
  Monitor,
  Film,
  ClipboardPaste,
  Home as HomeIcon,
  Download,
} from "lucide-react";
import {
  startCapture,
  captureDelayed,
  captureFull,
  repeatCapture,
  editClipboard,
  getSettings,
  notifyUser,
} from "@/lib/ipc";
import { checkUpdate, type UpdateInfo } from "@/lib/updater";
import { cn } from "@/lib/cn";
import TitleBar from "@/components/TitleBar";
import HistoryPanel from "@/features/home/HistoryPanel";
import SettingsPanel from "@/features/home/SettingsPanel";

type Page = "home" | "history" | "settings";

const NAV: { id: Page; icon: typeof HomeIcon; label: string }[] = [
  { id: "home", icon: HomeIcon, label: "截图" },
  { id: "history", icon: History, label: "历史" },
  { id: "settings", icon: SettingsIcon, label: "设置" },
];

export default function Home() {
  const [page, setPage] = useState<Page>("home");
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [focusUpdate, setFocusUpdate] = useState(false);
  const [checkNonce, setCheckNonce] = useState(0);

  const runUpdateCheck = useCallback(() => {
    const KEY = "snapix.updateCheckedAt";
    const last = Number(localStorage.getItem(KEY) ?? 0);
    // 6 小时节流：托盘常驻也能定期发现新版本
    if (Date.now() - last < 6 * 3600_000) return;
    checkUpdate()
      .then((info) => {
        localStorage.setItem(KEY, String(Date.now()));
        setUpdate(info);
        if (!info) return;
        // 主窗可能一直没打开，用系统通知提醒一次（每个版本只提醒一次）
        const NOTIFIED = "snapix.updateNotified";
        if (localStorage.getItem(NOTIFIED) !== info.version) {
          localStorage.setItem(NOTIFIED, info.version);
          notifyUser(`发现新版本 v${info.version}，打开 Snapix「设置 → 关于与更新」即可安装`).catch(
            () => {},
          );
        }
      })
      .catch(() => {
        // 网络 / 404 都按「稍后再试」处理，不要把 6 小时配额用掉
        localStorage.setItem(KEY, String(Date.now() - 5 * 3600_000));
      });
  }, []);

  // 启动 6 秒后查一次，之后每 30 分钟看一次是否到了 6 小时窗口。
  // 自动检查开关从设置里实时读取，关掉后不再发请求。
  useEffect(() => {
    const tick = async () => {
      try {
        const s = await getSettings();
        if (!s.updateCheck) return;
      } catch {
        // 读设置失败不阻塞检查，按默认开启处理
      }
      runUpdateCheck();
    };
    const first = window.setTimeout(() => void tick(), 6000);
    const timer = window.setInterval(() => void tick(), 30 * 60_000);
    return () => {
      window.clearTimeout(first);
      window.clearInterval(timer);
    };
  }, [runUpdateCheck]);

  // 托盘菜单 → 导航 / 检查更新
  useEffect(() => {
    const unNav = listen<string>("nav", (e) => {
      const p = e.payload as Page;
      if (p === "home" || p === "history" || p === "settings") {
        setFocusUpdate(false);
        setPage(p);
      }
    });
    const unCheck = listen("update:check", () => {
      setFocusUpdate(true);
      setCheckNonce((n) => n + 1);
      setPage("settings");
    });
    return () => {
      unNav.then((f) => f());
      unCheck.then((f) => f());
    };
  }, []);

  return (
    <div className="relative flex h-full flex-col overflow-hidden rounded-[18px] bg-[var(--shell)] text-white/90 ring-1 ring-white/[0.07]">
      {/* 氛围层：顶部一道极淡的天光 + 右下一点冷灰反光，让纯深色不显得「死板」 */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0"
        style={{
          background:
            "radial-gradient(90% 55% at 50% -12%, var(--ambient-1), transparent 62%)," +
            "radial-gradient(45% 38% at 88% 108%, var(--ambient-2), transparent 70%)",
        }}
      />
      {/* 顶部发丝高光：玻璃边缘的受光面 */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-10 top-0 h-px bg-gradient-to-r from-transparent via-white/20 to-transparent"
      />

      <div className="relative flex min-h-0 flex-1 flex-col">
        <TitleBar title="Snapix" />

        <div className="flex min-h-0 flex-1">
          {/* 侧边栏 */}
          <nav className="flex w-[68px] shrink-0 flex-col items-center gap-1 pt-2">
            {NAV.map((n) => {
              const active = page === n.id;
              return (
                <button
                  key={n.id}
                  onClick={() => {
                    setFocusUpdate(false);
                    setPage(n.id);
                  }}
                  title={n.label}
                  className={cn(
                    "flex h-[54px] w-[52px] flex-col items-center justify-center gap-1.5 rounded-xl transition-colors duration-200",
                    active
                      ? "bg-white/[0.07] text-white"
                      : "text-white/32 hover:bg-white/[0.035] hover:text-white/70",
                  )}
                >
                  <n.icon size={17} strokeWidth={active ? 1.9 : 1.6} />
                  <span className="text-[9.5px] tracking-[0.06em]">{n.label}</span>
                </button>
              );
            })}
          </nav>

          <main className="min-w-0 flex-1">
            {page === "home" && (
              <Dashboard
                update={update}
                onOpenSettings={() => {
                  setFocusUpdate(true);
                  setPage("settings");
                }}
              />
            )}
            {page === "history" && <HistoryPanel />}
            {page === "settings" && (
              <SettingsPanel focusUpdate={focusUpdate} checkNonce={checkNonce} />
            )}
          </main>
        </div>
      </div>
    </div>
  );
}

/* ===================== 首页 ===================== */

type Feature = { icon: typeof Camera; title: string; desc: string };

const FEATURES: Feature[] = [
  { icon: Camera, title: "区域截图", desc: "窗口吸附 · 微调 · 取色" },
  { icon: Film, title: "GIF 录屏", desc: "选区录制 · 自动保存" },
  { icon: Pin, title: "贴图置顶", desc: "缩放 · 旋转 · 穿透" },
  { icon: ScanText, title: "OCR / 二维码", desc: "离线识别 · 自动打码" },
  { icon: Sparkles, title: "出图美化", desc: "背景 · 圆角 · 水印" },
  { icon: ScrollText, title: "滚动长截图", desc: "网页 / 聊天记录拼接" },
  { icon: Monitor, title: "全屏即拍", desc: "整屏一键 · 瞬间复制" },
  { icon: History, title: "历史记录", desc: "自动留存 · 再编辑" },
];

function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="rounded-[5px] border border-white/10 bg-white/[0.05] px-1.5 py-[2px] font-sans text-[10.5px] font-medium text-white/55">
      {children}
    </kbd>
  );
}

function Dashboard({
  update,
  onOpenSettings,
}: {
  update: UpdateInfo | null;
  onOpenSettings: () => void;
}) {
  const [hotkey, setHotkey] = useState("Ctrl+Shift+A");
  const [version, setVersion] = useState("");
  // 轻提示：首页这些按钮都是「发出去就不管」的，失败时若静默会让人以为点了没反应
  const [tip, setTip] = useState<string | null>(null);
  const tipTimer = useRef<number | null>(null);

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  const hint = useCallback((text: string) => {
    setTip(text);
    if (tipTimer.current) window.clearTimeout(tipTimer.current);
    tipTimer.current = window.setTimeout(() => setTip(null), 2600);
  }, []);

  useEffect(
    () => () => {
      if (tipTimer.current) window.clearTimeout(tipTimer.current);
    },
    [],
  );

  useEffect(() => {
    getSettings().then((s) => s.hotkeyCapture && setHotkey(s.hotkeyCapture));
    const un = listen("nav", () => {
      getSettings().then((s) => s.hotkeyCapture && setHotkey(s.hotkeyCapture));
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <div className="flex h-full flex-col overflow-y-auto px-9 pb-3">
      {/* my-auto：内容不满一屏时上下均分留白，窗口再矮也不贴顶 */}
      <div className="my-auto flex flex-col">
        {/* Hero */}
        <section className="flex flex-col items-center pt-1 text-center">
          <motion.div
            initial={{ opacity: 0, scale: 0.94 }}
            animate={{ opacity: 1, scale: 1 }}
            transition={{ duration: 0.5, ease: [0.22, 1, 0.36, 1] }}
            className="relative"
          >
            {/* 光晕：只在图标周围散开一小圈，不要彩色 */}
            <div className="absolute -inset-5 rounded-full bg-white/[0.055] blur-2xl" />
            <div className="relative grid h-[52px] w-[52px] place-items-center rounded-[16px] border border-white/12 bg-gradient-to-b from-white/[0.13] to-white/[0.035] shadow-[inset_0_1px_0_rgba(255,255,255,0.14),0_10px_24px_-10px_rgba(0,0,0,0.9)]">
              <Camera size={23} strokeWidth={1.5} className="text-white/85" />
            </div>
          </motion.div>

          <h1 className="display mt-4 text-[21px] text-white/90">全能截图，从这里开始</h1>
          <p className="mt-1.5 max-w-sm text-[12.5px] leading-relaxed text-white/35">
            截图、标注、贴图、OCR 与美化 —— 安静地待命，随叫随到。
          </p>

          <div className="mt-5 flex flex-wrap items-center justify-center gap-2">
            <motion.button
              whileHover={{ y: -1 }}
              whileTap={{ scale: 0.985 }}
              onClick={() => startCapture()}
              className="inline-flex h-10 items-center gap-2 rounded-full bg-white px-5 text-[13px] font-semibold text-[var(--shell)] shadow-[0_8px_24px_-10px_rgba(255,255,255,0.5)] transition-colors hover:bg-white/90"
            >
              <Camera size={16} strokeWidth={2} />
              立即截图
            </motion.button>

            <QuickBtn icon={Monitor} title="全屏即拍：截取整屏并复制" onClick={() => captureFull()}>
              全屏
            </QuickBtn>
            <QuickBtn icon={Timer} title="延时 3 秒截图" onClick={() => captureDelayed(3)}>
              3 秒
            </QuickBtn>
            <QuickBtn icon={Timer} title="延时 10 秒截图" onClick={() => captureDelayed(10)}>
              10 秒
            </QuickBtn>
            <QuickBtn icon={RotateCw} title="重复上次区域" onClick={() => repeatCapture()}>
              上次区域
            </QuickBtn>
            <QuickBtn
              icon={ClipboardPaste}
              title="把剪贴板中的图片送进标注编辑器"
              onClick={() =>
                editClipboard().catch((e) => hint(`剪贴板里没有可用图片：${String(e)}`))
              }
            >
              标注剪贴板
            </QuickBtn>
          </div>

          <div className="mt-5 flex items-center gap-2 text-[11px] text-white/32">
            <Keyboard size={13} strokeWidth={1.6} />
            <span className="tracking-[0.02em]">全局快捷键</span>
            <span className="flex items-center gap-1">
              {hotkey.split("+").map((k, i) => (
                <span key={i} className="flex items-center gap-1">
                  {i > 0 && <span className="text-white/15">+</span>}
                  <Kbd>{k}</Kbd>
                </span>
              ))}
            </span>
          </div>
        </section>

        {/* 功能格：无卡片阴影、无彩色图标，只留发丝线与同色系的层次 */}
        <section className="mt-7 grid grid-cols-2 gap-2 sm:grid-cols-4">
          {FEATURES.map((f, i) => (
            <motion.div
              key={f.title}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.4, delay: 0.04 * i, ease: [0.22, 1, 0.36, 1] }}
              className="group rounded-[13px] border border-white/[0.055] bg-white/[0.022] px-3.5 py-3 transition-colors duration-200 hover:border-white/[0.11] hover:bg-white/[0.045]"
            >
              <div className="flex items-center gap-2.5">
                <f.icon
                  size={15}
                  strokeWidth={1.6}
                  className="shrink-0 text-white/35 transition-colors group-hover:text-white/70"
                />
                <h3 className="truncate text-[12.5px] font-medium text-white/75">{f.title}</h3>
              </div>
              <p className="mt-1.5 truncate pl-[25px] text-[10.5px] text-white/32">{f.desc}</p>
            </motion.div>
          ))}
        </section>
      </div>

      <footer className="mt-5 flex items-center justify-between gap-3 border-t border-white/[0.05] pt-3 text-[10.5px] text-white/25">
        <div className="flex min-w-0 items-center gap-2">
          <span className="shrink-0">Snapix{version ? ` v${version}` : ""}</span>
          {update && (
            <button
              onClick={onOpenSettings}
              title={`发现新版本 v${update.version}，前往设置更新`}
              className="inline-flex shrink-0 items-center gap-1 rounded-full border border-white/10 bg-white/[0.06] px-2 py-[2px] text-[10px] text-white/70 transition-colors hover:bg-white/[0.12] hover:text-white"
            >
              <Download size={10} strokeWidth={2} />
              新版本 v{update.version}
            </button>
          )}
        </div>
        <span className="truncate text-right">关闭窗口将收进托盘，快捷键持续可用</span>
      </footer>
      {tip && (
        <motion.div
          initial={{ opacity: 0, y: 8 }}
          animate={{ opacity: 1, y: 0 }}
          className="capsule pointer-events-none sticky bottom-2 mx-auto flex w-fit items-center gap-2 rounded-full px-3.5 py-1.5 text-[11.5px] text-white/70"
        >
          <span className="h-1.5 w-1.5 rounded-full bg-red-400/80" />
          {tip}
        </motion.div>
      )}
    </div>
  );
}

function QuickBtn({
  children,
  title,
  icon: Icon,
  onClick,
}: {
  children: React.ReactNode;
  title: string;
  icon: typeof Camera;
  onClick: () => void;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className="inline-flex h-10 items-center gap-1.5 rounded-full border border-white/[0.08] bg-white/[0.045] px-3.5 text-[12px] font-medium text-white/60 transition-colors duration-200 hover:border-white/[0.14] hover:bg-white/[0.09] hover:text-white/90"
    >
      <Icon size={14} strokeWidth={1.7} className="text-white/40" />
      {children}
    </button>
  );
}
