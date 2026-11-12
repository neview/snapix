import React from "react";
import ReactDOM from "react-dom/client";
import Line from "@/windows/Line";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Line />
    </ErrorBoundary>
  </React.StrictMode>,
);
