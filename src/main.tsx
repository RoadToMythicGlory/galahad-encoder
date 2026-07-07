import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { PreviewWindow } from "./PreviewWindow";
import "./styles.css";

const hashRoute = window.location.hash.replace(/^#\/?/, "");
const isPreview =
  hashRoute === "preview" ||
  new URLSearchParams(window.location.search).get("window") === "preview";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isPreview ? <PreviewWindow /> : <App />}</React.StrictMode>
);
