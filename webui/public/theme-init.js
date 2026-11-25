// 首屏主题引导（外部文件以满足 CSP script-src 'self'；不访问网络）。
(function () {
  try {
    var saved = localStorage.getItem('khaen.theme')
    var dark = saved ? saved === 'dark' : true
    document.documentElement.classList.toggle('dark', dark)
  } catch (e) {
    document.documentElement.classList.add('dark')
  }
})()
