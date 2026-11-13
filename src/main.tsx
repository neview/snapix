import React from "react";
import ReactDOM from "react-dom/client";
import Home from "@/windows/Home";
import ErrorBoundary from "@/components/ErrorBoundary";
import { initTheme } from "@/lib/theme";
import "@/styles/globals.css";

// 首帧前定主题：读本地缓存，同步执行，不会闪一下深色
initTheme();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Home />
    </ErrorBoundary>
  </React.StrictMode>,
);
