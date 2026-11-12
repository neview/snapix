import React from "react";
import ReactDOM from "react-dom/client";
import Editor from "@/windows/Editor";
import "@/styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Editor />
  </React.StrictMode>,
);
