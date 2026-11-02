// mdBook renders a ```mermaid block as <pre><code class="language-mermaid">; hand those to mermaid.
(function () {
  const dark = ["navy", "coal", "ayu"].some((t) => document.documentElement.classList.contains(t));
  document.querySelectorAll("code.language-mermaid").forEach((code) => {
    const div = document.createElement("div");
    div.className = "mermaid";
    div.textContent = code.textContent;
    code.parentElement.replaceWith(div);
  });
  mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "default" });
  mermaid.run({ querySelector: ".mermaid" });
})();
