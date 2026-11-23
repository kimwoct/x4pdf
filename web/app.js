(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const MAX_BYTES = 100 * 1024 * 1024;
  const POLL_MS = 1500;
  const MAX_POLL_FAILS = 6;
  const JOB_TIMEOUT_MS = 15 * 60 * 1000;

  const form = $("convertForm");
  const fileInput = $("fileInput");
  const dropzone = $("dropzone");
  const fileChip = $("fileChip");
  const fileName = $("fileName");
  const fileSize = $("fileSize");
  const clearFile = $("clearFile");
  const convertBtn = $("convertBtn");
  const formError = $("formError");
  const modeHint = $("modeHint");
  const orientation = $("orientation");
  const fontPt = $("fontPt");
  const fontRow = $("fontRow");
  const spreadRow = $("spreadRow");
  const spread = $("spread");
  const dither = $("dither");
  const progressCard = $("progressCard");
  const progressTitle = $("progressTitle");
  const progressText = $("progressText");
  const steps = Array.from(document.querySelectorAll(".step"));
  const resultCard = $("resultCard");
  const resultMeta = $("resultMeta");
  const downloadBtn = $("downloadBtn");
  const shareBtn = $("shareBtn");
  const againBtn = $("againBtn");
  const installBtn = $("installBtn");
  const netBadge = $("netBadge");
  const iosDialog = $("iosDialog");
  const iosClose = $("iosClose");
  const toast = $("toast");

  let file = null;
  let mode = "reflow";
  let jobId = null;
  let pollTimer = null;
  let pollFails = 0;
  let startedAt = 0;
  let deferredPrompt = null;
  let toastTimer = null;
  let lastResult = null;

  const isIOS = /iphone|ipad|ipod/i.test(navigator.userAgent);
  const isStandalone =
    window.matchMedia("(display-mode: standalone)").matches ||
    window.navigator.standalone === true;

  const prefs = readPrefs();
  if (prefs.mode === "pages") mode = "pages";
  if (["portrait", "landscape"].includes(prefs.orientation)) orientation.value = prefs.orientation;
  if (["10", "10.5", "11", "11.5", "12"].includes(String(prefs.fontPt))) {
    fontPt.value = String(prefs.fontPt);
  }
  if (["auto", "none", "fs"].includes(prefs.dither)) dither.value = prefs.dither;
  if (typeof prefs.spread === "boolean") spread.checked = prefs.spread;

  function readPrefs() {
    try {
      return JSON.parse(localStorage.getItem("x4pdf.prefs") || "{}");
    } catch {
      return {};
    }
  }

  function savePrefs() {
    try {
      localStorage.setItem(
        "x4pdf.prefs",
        JSON.stringify({
          mode,
          orientation: orientation.value,
          fontPt: fontPt.value,
          dither: dither.value,
          spread: spread.checked,
        })
      );
    } catch {
      /* private mode */
    }
  }

  function formatBytes(bytes) {
    if (!Number.isFinite(bytes) || bytes <= 0) return "";
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  }

  function showToast(message) {
    toast.textContent = message;
    toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      toast.hidden = true;
    }, 4200);
  }

  function setError(message) {
    formError.textContent = message;
    formError.hidden = !message;
  }

  function setMode(next) {
    mode = next;
    document.querySelectorAll(".seg").forEach((button) => {
      const active = button.dataset.mode === next;
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-selected", active ? "true" : "false");
    });
    const pages = next === "pages";
    fontRow.hidden = pages;
    spreadRow.hidden = !pages;
    modeHint.textContent = pages
      ? "Keeps the original page layout as 16-level images. Best for comics and magazines; no text search."
      : "Best for novels and dev books. Text reflows and stays searchable.";
    savePrefs();
  }

  function setFile(next) {
    if (!next) {
      file = null;
      fileInput.value = "";
      fileChip.hidden = true;
      convertBtn.disabled = true;
      return;
    }
    const isPdf =
      next.type === "application/pdf" || /\.pdf$/i.test(next.name || "");
    if (!isPdf) {
      setError("Choose a PDF file.");
      return;
    }
    if (next.size > MAX_BYTES) {
      setError("That PDF is larger than 100 MB.");
      return;
    }
    file = next;
    fileName.textContent = next.name;
    fileSize.textContent = formatBytes(next.size);
    fileChip.hidden = false;
    setError("");
    convertBtn.disabled = !navigator.onLine;
  }

  function setStep(name) {
    const order = ["upload", "convert", "ready"];
    const index = order.indexOf(name);
    steps.forEach((step) => {
      const stepIndex = order.indexOf(step.dataset.step);
      step.classList.toggle("is-done", stepIndex < index || (index === 2 && stepIndex === 2));
      step.classList.toggle("is-active", stepIndex === index && index < 2);
    });
  }

  function setBusy(busy) {
    convertBtn.disabled = busy || !file || !navigator.onLine;
    fileInput.disabled = busy;
    clearFile.disabled = busy;
    document.querySelectorAll(".seg").forEach((button) => {
      button.disabled = busy;
    });
  }

  function resetResult() {
    resultCard.hidden = true;
    resultMeta.textContent = "";
    downloadBtn.removeAttribute("href");
    downloadBtn.removeAttribute("download");
    shareBtn.hidden = true;
    lastResult = null;
  }

  function stopPolling() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  }

  async function startConversion() {
    if (!file) {
      setError("Choose a PDF first.");
      return;
    }
    if (!navigator.onLine) {
      setError("You are offline. Reconnect to the tailnet.");
      return;
    }

    setError("");
    resetResult();
    setBusy(true);
    progressCard.hidden = false;
    progressTitle.textContent = "Uploading…";
    progressText.textContent = "Sending the PDF to your Mac.";
    setStep("convert");
    startedAt = Date.now();

    const data = new FormData();
    data.append("file", file, file.name);
    data.append("orientation", orientation.value);
    data.append("font_pt", fontPt.value);
    data.append("rasterize", mode === "pages" ? "true" : "false");
    data.append("spread", mode === "pages" && spread.checked ? "true" : "false");
    data.append("dither", dither.value);

    try {
      const response = await fetch("/api/convert", { method: "POST", body: data });
      const raw = await response.text();
      let body = {};
      try {
        body = JSON.parse(raw);
      } catch {
        body = { message: raw };
      }
      if (!response.ok) {
        throw new Error(body.message || `Upload failed (${response.status})`);
      }
      if (!body.id) throw new Error("The server did not return a job id.");
      jobId = body.id;
      setStep("convert");
      progressTitle.textContent = mode === "pages" ? "Building page images…" : "Converting text…";
      progressText.textContent =
        mode === "pages"
          ? "Rendering 16-level pages at the reader size."
          : "Reflowing text, wrapping code and rendering figures.";
      pollFails = 0;
      pollTimer = setInterval(pollJob, POLL_MS);
      pollJob();
    } catch (error) {
      fail(error.message || "Could not upload the PDF.");
    }
  }

  async function pollJob() {
    if (!jobId) return;
    if (Date.now() - startedAt > JOB_TIMEOUT_MS) {
      stopPolling();
      fail("The conversion took too long. Try a smaller PDF or the page-image mode.");
      return;
    }

    try {
      const response = await fetch(`/api/jobs/${encodeURIComponent(jobId)}`, {
        cache: "no-store",
      });
      const raw = await response.text();
      let body = {};
      try {
        body = JSON.parse(raw);
      } catch {
        body = { message: raw };
      }
      if (!response.ok) throw new Error(body.message || `Status failed (${response.status})`);
      pollFails = 0;

      if (body.status === "queued") {
        progressTitle.textContent = "Queued…";
        progressText.textContent = "Waiting for the Mac to finish the current job.";
      } else if (body.status === "running") {
        progressTitle.textContent = mode === "pages" ? "Rendering pages…" : "Converting…";
        const seconds = Math.max(1, Math.round((Date.now() - startedAt) / 1000));
        progressText.textContent =
          mode === "pages"
            ? `Rendering grayscale pages · ${seconds}s`
            : `Reflowing text and figures · ${seconds}s`;
      } else if (body.status === "done") {
        stopPolling();
        showResult(body);
      } else if (body.status === "error") {
        stopPolling();
        fail(body.message || "Conversion failed.");
      }
    } catch (error) {
      pollFails += 1;
      if (pollFails >= MAX_POLL_FAILS) {
        stopPolling();
        fail(error.message || "Lost contact with the Mac.");
      }
    }
  }

  function fail(message) {
    stopPolling();
    progressCard.hidden = true;
    setBusy(false);
    setError(message);
    showToast(message);
  }

  function showResult(job) {
    setStep("ready");
    progressCard.hidden = true;
    setBusy(false);
    resultCard.hidden = false;
    const parts = [];
    if (job.pages) parts.push(`${job.pages} page${job.pages === 1 ? "" : "s"}`);
    if (job.bytes) parts.push(formatBytes(job.bytes));
    parts.push(`${orientation.value === "landscape" ? "800×480" : "480×800"} · 16 grays`);
    resultMeta.textContent = parts.join(" · ");
    lastResult = { url: job.download, filename: job.filename || "book.x4.epub" };
    downloadBtn.href = job.download;
    downloadBtn.download = lastResult.filename;
    shareBtn.hidden = !navigator.share;
    resultCard.scrollIntoView({ behavior: "smooth", block: "center" });
    showToast("EPUB ready.");
  }

  async function shareResult() {
    if (!lastResult) return;
    if (!navigator.share) {
      showToast("Sharing is not available; use Download.");
      return;
    }
    try {
      shareBtn.disabled = true;
      shareBtn.textContent = "Preparing…";
      const response = await fetch(lastResult.url, { cache: "no-store" });
      if (!response.ok) throw new Error("Download failed.");
      const blob = await response.blob();
      const shared = new File([blob], lastResult.filename, {
        type: "application/epub+zip",
      });
      if (!navigator.canShare || !navigator.canShare({ files: [shared] })) {
        downloadBtn.click();
        showToast("Sharing files is not supported here; downloading instead.");
        return;
      }
      await navigator.share({
        files: [shared],
        title: "X4C EPUB",
      });
    } catch (error) {
      if (error && error.name !== "AbortError") {
        showToast(error.message || "Could not share the EPUB.");
      }
    } finally {
      shareBtn.disabled = false;
      shareBtn.textContent = "Share";
    }
  }

  function resetAll() {
    stopPolling();
    jobId = null;
    setBusy(false);
    progressCard.hidden = true;
    resetResult();
    setFile(null);
    setError("");
    setMode("reflow");
    form.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  function updateNetwork() {
    const online = navigator.onLine;
    netBadge.hidden = online;
    convertBtn.disabled = !online || !file;
    if (!online) showToast("Offline — reconnect to the tailnet to convert.");
  }

  document.querySelectorAll(".seg").forEach((button) => {
    button.addEventListener("click", () => setMode(button.dataset.mode));
  });

  fileInput.addEventListener("change", () => {
    setFile(fileInput.files && fileInput.files[0]);
  });

  clearFile.addEventListener("click", () => {
    setFile(null);
  });

  dropzone.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      fileInput.click();
    }
  });

  ["dragenter", "dragover"].forEach((name) => {
    dropzone.addEventListener(name, (event) => {
      event.preventDefault();
      dropzone.classList.add("is-drag");
    });
  });
  ["dragleave", "drop"].forEach((name) => {
    dropzone.addEventListener(name, (event) => {
      event.preventDefault();
      if (name === "dragleave" && dropzone.contains(event.relatedTarget)) return;
      dropzone.classList.remove("is-drag");
      if (name === "drop" && event.dataTransfer && event.dataTransfer.files[0]) {
        setFile(event.dataTransfer.files[0]);
      }
    });
  });

  orientation.addEventListener("change", savePrefs);
  fontPt.addEventListener("change", savePrefs);
  dither.addEventListener("change", savePrefs);
  spread.addEventListener("change", savePrefs);

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    startConversion();
  });

  againBtn.addEventListener("click", resetAll);
  shareBtn.addEventListener("click", shareResult);

  window.addEventListener("online", updateNetwork);
  window.addEventListener("offline", updateNetwork);

  window.addEventListener("beforeinstallprompt", (event) => {
    event.preventDefault();
    deferredPrompt = event;
    if (!isStandalone) installBtn.hidden = false;
  });

  window.addEventListener("appinstalled", () => {
    installBtn.hidden = true;
    deferredPrompt = null;
    showToast("Installed.");
  });

  installBtn.addEventListener("click", async () => {
    if (deferredPrompt) {
      deferredPrompt.prompt();
      await deferredPrompt.userChoice.catch(() => null);
      deferredPrompt = null;
      installBtn.hidden = true;
      return;
    }
    if (isIOS) {
      if (typeof iosDialog.showModal === "function") iosDialog.showModal();
      else showToast("In Safari, Share → Add to Home Screen.");
      return;
    }
    showToast("Use your browser menu → Install app / Add to Home Screen.");
  });

  iosClose.addEventListener("click", () => iosDialog.close());
  iosDialog.addEventListener("click", (event) => {
    if (event.target === iosDialog) iosDialog.close();
  });

  if (!isStandalone && isIOS) installBtn.hidden = false;

  if ("serviceWorker" in navigator) {
    window.addEventListener("load", () => {
      navigator.serviceWorker.register("/sw.js", { updateViaCache: "none" }).catch(() => {
        /* offline support is optional */
      });
    });
  }

  setMode(mode);
  updateNetwork();
})();
