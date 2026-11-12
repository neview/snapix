import React from "react";
import ReactDOM from "react-dom/client";
import Home from "@/windows/Home";
import ErrorBoundary from "@/components/ErrorBoundary";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Home />
    </ErrorBoundary>
  </React.StrictMode>,
);
