import React from "react";
import ReactDOM from "react-dom/client";
import Pin from "@/windows/Pin";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Pin />
    </ErrorBoundary>
  </React.StrictMode>,
);
