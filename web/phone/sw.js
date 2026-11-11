/** Shows the box's pushes and opens the chat a tapped one is about. */
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("push", (e) => {
  let d = {};
  try {
    d = e.data.json();
  } catch {
    d = { body: e.data ? e.data.text() : "" };
  }
  e.waitUntil(
    self.registration.showNotification(d.title || "Turtwig", { body: d.body || "", tag: d.tag, data: { chat: d.chat }, icon: "/icon-180.png" })
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const go = e.notification.data && e.notification.data.chat ? `#/chat/${e.notification.data.chat}` : "#/inbox";
  e.waitUntil(
    (async () => {
      const open = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      if (open.length) {
        await open[0].focus();
        open[0].postMessage({ go });
        return;
      }
      await self.clients.openWindow(`/${go}`);
    })()
  );
});
