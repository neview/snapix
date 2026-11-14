import { Component, type ReactNode } from "react";

type Props = {
  children: ReactNode;
  /**
   * 出错时的额外收尾。遮罩窗必须传：它是一块整屏置顶窗，渲染树一崩，
   * 用户连关都关不掉——只能去托盘强杀进程。
   */
  onError?: (error: Error) => void;
};
type State = { error: Error | null };

/**
 * 单个窗口的渲染兜底：任何子组件抛错都停在这里，而不是留一块空白 / 透明窗。
 * 只影响出错的窗口，其他窗口和托盘继续工作；重新加载即可恢复。
 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: unknown) {
    console.error("[snapix] 窗口渲染失败", error, info);
    try {
      this.props.onError?.(error);
    } catch {
      // 收尾本身出错不能再抛，否则整棵错误树会再崩一次
    }
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="grid h-screen w-screen place-items-center bg-[var(--shell)] p-6 text-center text-white/80">
        <div>
          <div className="text-[13px]">这个窗口出了点问题</div>
          <p className="mx-auto mt-1.5 max-w-xs truncate text-[11px] text-white/35">
            {String(this.state.error)}
          </p>
          <button
            onClick={() => window.location.reload()}
            className="mt-3 rounded-full bg-white px-4 py-1.5 text-[12px] font-medium text-black transition-colors hover:bg-white/85"
          >
            重新加载
          </button>
        </div>
      </div>
    );
  }
}
