// Theme switch: follows the device until clicked, then remembers the choice.
const root = document.documentElement;
const toggle = document.querySelector(".theme-toggle");
const prefersDark = matchMedia("(prefers-color-scheme: dark)");
const currentTheme = () => root.dataset.theme || (prefersDark.matches ? "dark" : "light");

function showToggle() {
  const next = currentTheme() === "dark" ? "light" : "dark";
  toggle.setAttribute("aria-label", `Switch to ${next} theme`);
}

toggle.hidden = false;
showToggle();
prefersDark.addEventListener("change", showToggle);
toggle.addEventListener("click", () => {
  const next = currentTheme() === "dark" ? "light" : "dark";
  root.dataset.theme = next;
  try { localStorage.setItem("theme", next); } catch (e) {}
  showToggle();
});

// Connection methods: one panel open at a time.
const tabs = [...document.querySelectorAll(".method")];
function select(tab, focus) {
  for (const t of tabs) {
    const on = t === tab;
    t.setAttribute("aria-selected", on);
    t.tabIndex = on ? 0 : -1;
    document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
  }
  if (focus) tab.focus();
}
tabs.forEach((tab, i) => {
  tab.addEventListener("click", () => select(tab, false));
  tab.addEventListener("keydown", (e) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (step) {
      e.preventDefault();
      select(tabs[(i + step + tabs.length) % tabs.length], true);
    }
  });
});
select(tabs.find((t) => t.getAttribute("aria-selected") === "true") || tabs[0], false);

// Fade sections in as they scroll into view.
const reveals = document.querySelectorAll(".reveal");
if ("IntersectionObserver" in window) {
  const seen = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        entry.target.classList.add("in");
        seen.unobserve(entry.target);
      }
    }
  }, { rootMargin: "0px 0px -8% 0px" });
  reveals.forEach((el) => seen.observe(el));
} else {
  reveals.forEach((el) => el.classList.add("in"));
}

// Latest release, from GitHub. Stays hidden until there is one.
const version = document.querySelector(".version");
async function showVersion() {
  let tag = null;
  try { tag = sessionStorage.getItem("release"); } catch (e) {}
  if (!tag) {
    const response = await fetch("https://api.github.com/repos/Wilsoon7721/WinDroidFS/releases/latest");
    if (!response.ok) return;
    tag = (await response.json()).tag_name;
    if (!tag) return;
    try { sessionStorage.setItem("release", tag); } catch (e) {}
  }
  version.textContent = `Latest release: ${tag}.`;
  version.hidden = false;
}
showVersion().catch(() => {});
