import React from "react";
import ReactDOM from "react-dom/client";
import Editor from "@/windows/Editor";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Editor />
    </ErrorBoundary>
  </React.StrictMode>,
);
