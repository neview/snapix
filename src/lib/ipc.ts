import { invoke } from "@tauri-apps/api/core";

/** 可吸附窗口（显示器相对物理坐标，z 序自顶向下）。 */
export type SnapWindow = {
  title: string;
  x: number;
  y: number;
  w: number;
  h: number;
};

/** 选区矩形（显示器相对物理像素）。 */
export type SelRect = { x: number; y: number; w: number; h: number };

/** 遮罩打开时携带的冻屏数据。 */
export type OverlayPayload = {
  /** 目标遮罩窗标签（多屏防串扰校验） */
  label: string;
  frameId: number;
  frameUrl: string;
  width: number;
  height: number;
  scale: number;
  windows: SnapWindow[];
  preselect: SelRect | null;
};

/** 编辑器 / 贴图窗口装载的图像。 */
export type ImagePayload = {
  imageUrl: string;
  width: number;
  height: number;
  scale: number;
  displayWidth?: number;
  displayHeight?: number;
  /** 该贴图是从晾衣绳取下的：双击 = 挂回绳子（而不是关闭）。 */
  fromLine?: boolean;
};

/** 应用设置（与 Rust settings.rs 对应）。 */
export type Settings = {
  hotkeyCapture: string;
  hotkeyRepeat: string;
  hotkeyPinClip: string;
  hotkeyFull: string;
  hotkeyWindow: string;
  hotkeyTogglePins: string;
  saveDir: string;
  fileTemplate: string;
  imageFormat: "png" | "jpg";
  saveAsk: boolean;
  copyAfterSave: boolean;
  autostart: boolean;
  historyEnabled: boolean;
  historyLimit: number;
  /** 截图产出自动挂到顶部「晾衣绳」 */
  lineEnabled: boolean;
  /** 启动后自动检查更新 */
  updateCheck: boolean;
  /** 截图是否把鼠标光标一起拍进去 */
  captureCursor: boolean;
  /** 主题：dark | light | system */
  theme: "dark" | "light" | "system";
};

/** 历史记录条目。 */
export type HistoryEntry = {
  id: string;
  ts: number;
  width: number;
  height: number;
  source: "capture" | "editor";
  url: string;
};

// ===================== 截图 =====================

/** 触发一次截图：显示全屏遮罩选区窗。 */
export function startCapture() {
  return invoke("start_capture");
}

/** 取消/关闭当前截图遮罩。 */
export function cancelCapture() {
  return invoke("cancel_capture");
}

/** 发一条系统通知（如发现新版本）。 */
export function notifyUser(body: string) {
  return invoke("notify_user", { body });
}

/** 打开日志目录。 */
export function openLogDir() {
  return invoke("open_log_dir");
}

/** 冻屏图已渲染完毕，通知后端显示遮罩窗。 */
export function overlayReady(frameId: number) {
  return invoke("overlay_ready", { frameId });
}

/** 遮罩窗挂载时主动拉取待处理载荷（防 emit 早于监听器注册）。 */
export function overlayFetch() {
  return invoke<OverlayPayload | null>("overlay_fetch");
}

/** 本遮罩窗开始交互，通知其他屏遮罩清空选区。 */
export function overlayClaim() {
  return invoke("overlay_claim");
}

/**
 * 本帧已用完，请收起遮罩。带帧号与状态校验：新一轮截图已经接管时是空操作，
 * 所以可以安全地作为「第一次收遮罩失败」的兜底补刀。
 */
export function overlayRelease(frameId: number) {
  return invoke("overlay_release", { frameId });
}

/** 延时截图。 */
export function captureDelayed(secs: number) {
  return invoke("capture_delayed", { secs });
}

/** 全屏即拍：截取光标所在屏 → 剪贴板（依设置可同时直存）。 */
export function captureFull() {
  return invoke("capture_full");
}

/** 窗口即拍：截取前台窗口 → 剪贴板（依设置可同时直存）。 */
export function captureWindow() {
  return invoke("capture_window");
}

/** 重复上次区域截图。 */
export function repeatCapture() {
  return invoke("repeat_capture");
}

/** 裁剪选区并复制到剪贴板（坐标为物理像素）。 */
export function copyRegion(x: number, y: number, width: number, height: number) {
  return invoke("copy_region", { x, y, width, height });
}

/** 飞向晾衣绳的落点（遮罩窗相对 CSS 像素）。 */
export type LineLanding = {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
};

/**
 * 复制 + 挂到晾衣绳，但**不立刻收起遮罩**：返回绳上的落点，让遮罩播放
 * 「从选区飞到绳上」的过渡，结束后再调 `lineLanded` 收尾。
 * 返回 null 表示本次不飞行（晾衣绳关闭 / 跨屏 / 图片被收纳），遮罩已由后端收起。
 */
export function copyToLine(x: number, y: number, width: number, height: number) {
  return invoke<LineLanding | null>("copy_to_line", { x, y, width, height });
}

/** 飞行动画播完：绳上条目正式出现、遮罩收起。`id` 为空表示放弃动画（兜底收起遮罩）。 */
export function lineLanded(id: string) {
  return invoke("line_landed", { id });
}

/** 裁剪选区并保存（依设置询问或直存），返回保存路径。 */
export function saveRegion(x: number, y: number, width: number, height: number) {
  return invoke<string | null>("save_region", { x, y, width, height });
}

/** 裁剪选区并打开标注编辑器。 */
export function openEditor(x: number, y: number, width: number, height: number) {
  return invoke("open_editor", { x, y, width, height });
}

/** 裁剪选区并在原位置生成置顶贴图窗。 */
export function pinRegion(x: number, y: number, width: number, height: number) {
  return invoke("pin_region", { x, y, width, height });
}

/** 对选区做离线 OCR，返回识别文本。 */
export function ocrRegion(x: number, y: number, width: number, height: number) {
  return invoke<string>("ocr_region", { x, y, width, height });
}

/** 识别选区内的二维码，返回全部解码文本。 */
export function qrRegion(x: number, y: number, width: number, height: number) {
  return invoke<string[]>("qr_region", { x, y, width, height });
}

/** 光标下 UI 元素命中结果（显示器相对物理像素）。 */
export type ElementHit = SelRect & { name: string };

/** 查询光标下 UI 元素的可视边界（控件级吸附）。 */
export function elementAt() {
  return invoke<ElementHit | null>("element_at");
}

/** 对编辑器当前图像做隐私检测，返回需打码区域（图像坐标）。 */
export function detectPrivacy() {
  return invoke<SelRect[]>("editor_detect_privacy");
}

/** 把剪贴板中的图片贴为置顶贴图。 */
export function pinClipboard() {
  return invoke("pin_clipboard");
}

/** 把剪贴板中的图片送进标注编辑器。 */
export function editClipboard() {
  return invoke("edit_clipboard");
}

/** 一键隐藏 / 显示全部贴图窗口。 */
export function togglePins() {
  return invoke("toggle_pins");
}

// ===================== 滚动长截图 =====================

/** 滚动截图进度事件载荷。 */
export type ScrollProgress = {
  /** 横向滚动（控制条据此改文案） */
  horizontal: boolean;
  heightPx: number;
  frames: number;
  /** 已暂停，等待手动步进 */
  paused: boolean;
  /** 每帧滚轮档数 */
  notches: number;
  /** 需要提示的异常（匹配失败 / 疑似触底） */
  warn?: string;
};

/** 对选区开始滚动长截图（自动滚动 + 拼接）。 */
export function scrollCaptureStart(
  x: number,
  y: number,
  width: number,
  height: number,
  horizontal = false,
) {
  return invoke("scroll_capture_start", { x, y, width, height, horizontal });
}

/** 结束滚动截图并装配长图进编辑器。 */
export function scrollCaptureStop() {
  return invoke("scroll_capture_stop");
}

/** 取消滚动截图。 */
export function scrollCaptureCancel() {
  return invoke("scroll_capture_cancel");
}

/** 暂停 / 继续自动滚动（暂停后可逐帧步进）。 */
export function scrollCaptureSetPaused(paused: boolean) {
  return invoke("scroll_capture_set_paused", { paused });
}

/** 手动步进一帧（内部会先进入暂停态）。 */
export function scrollCaptureStep(notches = 1) {
  return invoke("scroll_capture_step", { notches });
}

// ===================== GIF 录制 =====================

/** GIF 录制进度事件载荷。 */
export type GifProgress = { secs: number; frames: number };

/** 对选区开始 GIF 录制（10fps，停止后自动保存并定位文件）。 */
export function gifRecordStart(x: number, y: number, width: number, height: number) {
  return invoke("gif_record_start", { x, y, width, height });
}

/** 停止录制并保存 GIF。 */
export function gifRecordStop() {
  return invoke("gif_record_stop");
}

/** 取消录制并丢弃。 */
export function gifRecordCancel() {
  return invoke("gif_record_cancel");
}

// ===================== 编辑器 =====================

/** 把标注成图（data URL）复制到剪贴板。 */
export function copyImage(dataUrl: string) {
  return invoke("copy_image", { dataUrl });
}

/** 把标注成图（data URL）保存，返回路径。 */
export function saveImage(dataUrl: string) {
  return invoke<string | null>("save_image", { dataUrl });
}

/** 关闭标注编辑器。 */
export function closeEditor() {
  return invoke("close_editor");
}

/** 标注窗口按需创建：挂载时拉取待标注图（事件可能早于监听）。 */
export function editorPending() {
  return invoke<ImagePayload | null>("editor_pending");
}

/** 首帧画好后通知后端显示标注窗口（新建窗口默认隐藏，避免白闪）。 */
export function editorReady() {
  return invoke("editor_ready");
}

// ===================== 贴图 =====================

/** 贴图窗口挂载时获取自己的图。 */
export function getPinImage(label: string) {
  return invoke<ImagePayload | null>("get_pin_image", { label });
}

/** 贴图图片已渲染完毕，通知后端在目标位置显示窗口（仅首次生效）。 */
export function pinReady() {
  return invoke("pin_ready");
}

/** 复制贴图内容。 */
export function pinCopy(label: string) {
  return invoke("pin_copy", { label });
}

/** 另存贴图内容，返回路径。 */
export function pinSave(label: string) {
  return invoke<string | null>("pin_save", { label });
}

/** 贴图旋转 / 翻转后的新尺寸。 */
export type PinDims = { width: number; height: number };

/** 贴图顺时针旋转 90°，返回新尺寸。 */
export function pinRotate(label: string) {
  return invoke<PinDims>("pin_rotate", { label });
}

/** 贴图水平镜像翻转，返回尺寸。 */
export function pinFlip(label: string) {
  return invoke<PinDims>("pin_flip", { label });
}

/** 贴图开启鼠标穿透（解除需托盘菜单）。 */
export function pinPassthrough(label: string) {
  return invoke("pin_passthrough", { label });
}

/** 把贴图内容送进标注编辑器。 */
export function pinAnnotate(label: string) {
  return invoke("pin_annotate", { label });
}

/** 把标注成图（data URL）直接生成贴图。 */
export function pinData(dataUrl: string) {
  return invoke("pin_data", { dataUrl });
}

/** 关闭某个贴图窗口。 */
export function closePin(label: string) {
  return invoke("close_pin", { label });
}

/** 复制纯文本到剪贴板。 */
export function copyText(text: string) {
  return invoke("copy_text", { text });
}

// ===================== 设置 =====================

export function getSettings() {
  return invoke<Settings>("get_settings");
}

export function updateSettings(settings: Settings) {
  return invoke("update_settings", { settings });
}

/** 恢复全部默认设置，返回重置后的快照。 */
export function resetSettings() {
  return invoke<Settings>("reset_settings");
}

/** 弹出目录选择框，返回所选路径。 */
export function pickFolder() {
  return invoke<string | null>("pick_folder");
}

/** 录制快捷键期间暂停全局热键。 */
export function hotkeysSuspend() {
  return invoke("hotkeys_suspend");
}

/** 录制结束后恢复全局热键。 */
export function hotkeysResume() {
  return invoke("hotkeys_resume");
}

// ===================== 历史 =====================

export function historyList() {
  return invoke<HistoryEntry[]>("history_list");
}

export function historyDelete(id: string) {
  return invoke("history_delete", { id });
}

export function historyClear() {
  return invoke("history_clear");
}

export function historyOpenDir() {
  return invoke("history_open_dir");
}

export function historyCopy(id: string) {
  return invoke("history_copy", { id });
}

export function historySaveAs(id: string) {
  return invoke<string | null>("history_save_as", { id });
}

export function historyPin(id: string) {
  return invoke("history_pin", { id });
}

export function historyEdit(id: string) {
  return invoke("history_edit", { id });
}

// ===================== 晾衣绳 =====================

/** 绳子上的图。x 为沿绳归一化位置，null = 自动排布。 */
export type LineItem = {
  id: string;
  ts: number;
  width: number;
  height: number;
  x: number | null;
  /** 正在飞过来的途中：位置已经算好，但要等落地动画结束才显示 */
  pending?: boolean;
  url: string;
};

export function lineList() {
  return invoke<LineItem[]>("line_list");
}

export function lineRemove(id: string) {
  return invoke("line_remove", { id });
}

/** 沿绳拖动后回写位置（`null` = 回到自动排布）。 */
export function lineMove(id: string, x: number | null) {
  return invoke("line_move", { id, x });
}

/** 拖动开始 / 结束：拖动期间后端钉住可点状态，别把鼠标漏给下层窗口。 */
export function lineDragging(dragging: boolean) {
  return invoke("line_dragging", { dragging });
}

export function lineClear() {
  return invoke("line_clear");
}

export function lineCopy(id: string) {
  return invoke("line_copy", { id });
}

export function lineSave(id: string) {
  return invoke<string | null>("line_save", { id });
}

export function linePin(id: string) {
  return invoke("line_pin", { id });
}

/** 上报可点区域（窗口相对物理像素），供后端开关鼠标穿透。 */
export function lineHitRects(rects: [number, number, number, number][]) {
  return invoke("line_hit_rects", { rects });
}

export function lineItemReady(id: string, ready: boolean) {
  return invoke("line_item_ready", { id, ready });
}

/**
 * 取下的终点矩形（虚拟桌面物理像素）：屏幕中间偏下、宽度取工作区 42%。
 * 动画由前端在条窗里播，这里只问「终点在哪」，不碰任何窗口。
 */
export function lineDetachTarget(x: number, y: number, width: number, height: number) {
  return invoke<[number, number, number, number]>("line_detach_target", { x, y, width, height });
}

export function linePrepareDetach(id: string, x: number, y: number, width: number, height: number) {
  return invoke<string>("line_prepare_detach", { id, x, y, width, height });
}

export function lineRevealDetach(id: string, label: string) {
  return invoke("line_reveal_detach", { id, label });
}

export function lineFinishDetach(id: string, label: string) {
  return invoke("line_finish_detach", { id, label });
}

export function lineCancelDetach(id: string) {
  return invoke("line_cancel_detach", { id });
}

/**
 * 贴图「挂回绳子」的飞行：起点是贴图窗内容矩形（屏幕物理像素），
 * 终点是绳窗内落点（CSS 像素）。由绳窗监听 `line:return` 事件接手。
 */
export type LineReturnFlight = {
  id: string;
  label: string;
  url: string;
  from: [number, number, number, number];
  to: [number, number, number, number];
};

/** 双击贴图：挂回晾衣绳。`true` = 会播飞行动画；`false` = 不在绳窗范围内，已直接收回。 */
export function lineReturnPin(label: string) {
  return invoke<boolean>("line_return_pin", { label });
}

/** 绳窗已画好飞行首帧：关掉贴图窗，由飞行层接管画面。 */
export function lineReturnTakeoff(id: string, label: string) {
  return invoke("line_return_takeoff", { id, label });
}

/** 飞行落地：绳上真图显形并释放贴图窗。 */
export function lineReturnLanded(id: string, label: string) {
  return invoke("line_return_landed", { id, label });
}

/**
 * 截图时把晾衣架举到遮罩之上（选区拖到屏幕顶端时显示），离开就收回。
 * 只改 z 序与显隐，不动几何，所以不会让正在进行的拖选掉帧。
 */
export function lineDropHint(on: boolean) {
  return invoke("line_drop_hint", { on });
}
