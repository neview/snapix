import React from "react";
import ReactDOM from "react-dom/client";
import Gif from "@/windows/Gif";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Gif />
    </ErrorBoundary>
  </React.StrictMode>,
);
