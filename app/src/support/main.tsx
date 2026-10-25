import React from "react";
import { createRoot } from "react-dom/client";
import { applyAppearance } from "../appearance";
import { SupportWindow } from "./SupportWindow";
import "../components/icon-glyphs/shell";
import "../foundation.css";
import "../owned-utilities.css";
import "../octopus-owned-components.css";
import "../styles.css";
import "../theme.css";

applyAppearance();
createRoot(document.getElementById("root")!).render(<React.StrictMode><SupportWindow /></React.StrictMode>);
