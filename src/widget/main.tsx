import React from "react";
import ReactDOM from "react-dom/client";
import "../tokens.css";
import "../index.css";
import "./widget.css";
import { WidgetApp } from "./WidgetApp";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <WidgetApp />
  </React.StrictMode>,
);
