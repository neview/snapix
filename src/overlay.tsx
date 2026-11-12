import React from "react";
import ReactDOM from "react-dom/client";
import Overlay from "@/windows/Overlay";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Overlay />
    </ErrorBoundary>
  </React.StrictMode>,
);
