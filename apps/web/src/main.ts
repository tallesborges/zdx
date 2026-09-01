import { mount } from "svelte";
import App from "./App.svelte";
import { initTelegram } from "./lib/telegram";
import "./app.css";

initTelegram();

const target = document.getElementById("app");
if (!target) throw new Error("#app mount target missing");

export default mount(App, { target });
