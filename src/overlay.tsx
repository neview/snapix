import React from "react";
import ReactDOM from "react-dom/client";
import Overlay from "@/windows/Overlay";
import ErrorBoundary from "@/components/ErrorBoundary";
import { cancelCapture } from "@/lib/ipc";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary
      // 遮罩渲染崩了：先把全屏置顶窗收起来，再让兜底页留在后台。
      // 否则用户面对的是一块既没有画面、又吃掉所有点击的透明窗。
      onError={() => {
        void cancelCapture().catch(() => {});
      }}
    >
      <Overlay />
    </ErrorBoundary>
  </React.StrictMode>,
);
