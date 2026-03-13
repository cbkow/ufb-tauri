/* @refresh reload */
import { render } from "solid-js/web";
import "material-symbols/outlined.css";
import App from "./App";

// Disable the default browser context menu globally
document.addEventListener("contextmenu", (e) => e.preventDefault());

render(() => <App />, document.getElementById("root") as HTMLElement);
