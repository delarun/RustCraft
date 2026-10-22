// Русские сообщения (тип Messages: пропущенный или лишний ключ не пройдёт typecheck).

import type { Messages } from './en';

type N = number;
type S = string;

export const ru: Messages = {
  // --- header ---
  clickToCaptureMouse: 'Щёлкните, чтобы управлять мышью',
  language: 'Язык',
  tagline: 'Ваша копия Minecraft: Java Edition прямо в браузере — на Java-машине, написанной на Rust.',

  // --- account ---
  accountTitle: 'Аккаунт',
  accountIntro: 'Для игры нужна лицензия Minecraft: Java Edition. Войдите аккаунтом Microsoft, на котором куплена игра.',
  noClientIdNotice: 'В этой сборке не настроен вход Microsoft: не задан client ID приложения (VITE_MS_CLIENT_ID).',
  signIn: 'Войти через Microsoft',
  deviceOpenBefore: 'Откройте',
  deviceOpenAfter: '(на этом или другом устройстве) и введите код:',
  copy: 'Копировать',
  cancel: 'Отмена',
  signOut: 'Выйти',
  signInSettings: 'Настройки входа',
  clientIdLabel: 'Azure client ID',
  save: 'Сохранить',
  stepMsa: 'Обновляю вход Microsoft…',
  stepXbox: 'Вход в Xbox Live…',
  stepXsts: 'Авторизация XSTS…',
  stepMinecraft: 'Вход в Minecraft…',
  stepLicense: 'Проверяю лицензию…',
  stepProfile: 'Загружаю профиль…',
  finishSignIn: 'Завершите вход в аккаунт Microsoft.',
  signInToPlay: 'Войдите аккаунтом Microsoft с купленной Minecraft: Java Edition.',
  signInUnavailable: 'Вход Microsoft недоступен: не задан client ID.',
  notSignedIn: 'Не выполнен вход',
  requestingCode: 'Запрашиваю код входа…',
  waitingForSignIn: 'Жду, пока вы войдёте на странице Microsoft…',
  codeExpiresIn: (minutes: N, seconds: S) => `Код действует ещё ${minutes}:${seconds}`,
  signInCancelled: 'Вход отменён.',
  signedOut: 'Вы вышли из аккаунта. Токены удалены из браузера.',
  clientIdInvalid: 'Client ID — это GUID вида 00000000-0000-0000-0000-000000000000.',
  clientIdFromSettings: (id: S) => `Используется client ID ${id} (из настроек этого браузера).`,
  clientIdFromBuild: (id: S) => `Используется client ID ${id} (из сборки, VITE_MS_CLIENT_ID).`,
  clientIdDefault: (id: S) => `Используется собственный client ID RustCraft: ${id}.`,
  noClientId:
    'В этой сборке не задан client ID приложения Microsoft (VITE_MS_CLIENT_ID), поэтому вход недоступен. ' +
    'Администратору сайта: см. README.md; для проверки можно ввести свой client ID в «Настройках входа».',
  noJavaProfile: 'без профиля Java Edition',
  licenseOk: '✓ Лицензия Minecraft: Java Edition подтверждена',
  noLicense: 'У этого аккаунта нет лицензии Minecraft: Java Edition. Купите игру на minecraft.net и войдите снова.',
  noProfile: 'Игра куплена, но профиль ещё не создан: выберите ник на minecraft.net и войдите снова.',
  licenseNotFound: 'Лицензия Minecraft: Java Edition не найдена.',

  // --- sign-in errors ---
  network: (host: S, message: S) => `Сеть недоступна (${host}): ${message}`,
  msaDeclined: 'Вход отменён в окне Microsoft.',
  msaExpired: 'Код входа истёк. Начните вход заново.',
  msaRelogin: 'Сессия Microsoft истекла, войдите заново.',
  msaAppNotFound: 'Приложение Azure с таким client ID не найдено (проверьте client ID в настройках).',
  msaPublicFlows: 'В приложении Azure не включены «public client flows» (нужны для входа по коду).',
  msaError: (description: S) => `Microsoft: ${description}`,
  xblFailed: (status: N) => `Xbox Live: вход не удался (HTTP ${status}).`,
  xstsBanned: 'Этот аккаунт Xbox заблокирован.',
  xstsChild: 'Аккаунт детский: онлайн-доступ должен разрешить взрослый в настройках семьи Microsoft.',
  xstsNoXbox: 'У аккаунта Microsoft нет профиля Xbox. Создайте его на xbox.com и войдите снова.',
  xstsTerms: 'Примите условия Xbox на xbox.com и войдите снова.',
  xstsCountry: 'Xbox Live недоступен в стране аккаунта.',
  xstsAgeVerification: 'Аккаунту нужна проверка возраста (Южная Корея) на xbox.com.',
  xstsMinor: 'Аккаунт несовершеннолетнего: его нужно добавить в семью Microsoft взрослым.',
  xstsFailed: (status: N, xerr: S) => `Xbox XSTS: авторизация не удалась (HTTP ${status}${xerr ? `, XErr ${xerr}` : ''}).`,
  xboxNoUserHash: 'Xbox Live: в ответе нет userhash.',
  mcNotApproved:
    'Minecraft отклонил вход: это приложение Azure не одобрено Mojang для Minecraft API ' +
    '(https://aka.ms/mce-reviewappid). Используйте одобренный client ID.',
  mcRateLimited: 'Слишком много попыток входа в Minecraft. Подождите минуту.',
  mcLoginFailed: (status: N, message: S) => `Minecraft: вход не удался (HTTP ${status}${message ? `: ${message}` : ''}).`,
  entitlementsFailed: (status: N) => `Minecraft: не удалось проверить лицензию (HTTP ${status}).`,
  profileFailed: (status: N) => `Minecraft: не удалось получить профиль (HTTP ${status}).`,

  // --- version ---
  versionTitle: 'Версия',
  versionAria: 'Версия игры',
  snapshots: 'снапшоты',
  memoryLabel: 'Память Java',
  memory1g: '1 ГБ',
  memory15g: '1,5 ГБ',
  memory2g: '2 ГБ',
  loadingVersions: 'Загружаю список версий…',
  manifestFailed: (error: S) => `Не удалось загрузить список версий с piston-meta.mojang.com: ${error}`,
  latestRelease: ' (последний релиз)',
  latestSnapshot: ' (последний снапшот)',
  markTested: ' ✓ проверена',
  markUnsupported: ' — не поддерживается',
  checkingCompat: 'Проверяю совместимость…',
  compatTested: 'проверена на нашей JVM',
  compatUntested: 'должна запускаться, но не проверялась',
  compatJava: (java: N, supported: N) => `нужна Java ${java}, наша JVM пока работает только с библиотекой классов Java ${supported}`,
  compatGlfw: 'версия использует GLFW, а наша JVM реализует только SDL-окна LWJGL',
  compatDownload: (java: S) => ` Игра и ${java} скачаются с серверов Mojang при первом запуске.`,
  versionJsonFailed: (error: S) => `Не удалось загрузить описание версии: ${error}`,

  // --- play ---
  playTitle: 'Игра',
  eulaHtml:
    'Я прочитал(а) и принимаю <a href="https://www.minecraft.net/eula" target="_blank" rel="noopener noreferrer">лицензионное соглашение (EULA) Minecraft</a>',
  play: 'Играть',
  verifyFiles: 'Проверить файлы',
  failuresSummary: 'Ошибки загрузки',
  browserMissing: (features: S) => `Браузер не поддерживает: ${features}.`,
  blockedIsolation: 'Игра здесь не запустится: страница не изолирована (cross-origin isolation) — сервер должен отдавать заголовки COOP/COEP.',
  blockedSharedMemory: 'Игра не запустится в этом браузере: нет поддержки SharedArrayBuffer. Попробуйте свежий Chrome, Edge или Firefox.',
  featureMissing: (feature: S, why: S) => `${feature} недоступен в этом браузере: ${why}.`,
  whyIsolation: 'страница должна отдаваться с заголовками COOP/COEP',
  whySharedMemory: 'нужен для потоков JVM',
  whyOpfs: 'нужно для хранения файлов игры',
  whyWebgl: 'нужен для графики',
  whyAudio: 'нужен для звука',
  noManifest: 'Список версий не загружен.',
  versionUnsupported: (id: S) => `Версия ${id} не запустится на нашей JVM.`,
  acceptEula: 'Чтобы играть, примите EULA Minecraft.',
  untestedWarning: 'Эта версия не проверялась: возможны ошибки.',
  checkingSignIn: 'Проверяю вход…',
  versionJsonMissing: 'Описание версии не сохранилось в хранилище',
  gameExited: (code: N) => `Игра завершилась (код ${code}).`,

  // --- install ---
  installMetadata: (id: S) => `Загружаю метаданные ${id}`,
  installRuntime: (component: S) => `Загружаю описание Java-рантайма ${component}`,
  installChecking: (full: boolean, files: N) => `${full ? 'Проверяю (SHA-1)' : 'Проверяю'} и скачиваю ${files} файлов`,
  installBusy: 'Установка уже идёт',
  installCancelled: 'Установка отменена',
  installFailed: (count: N) => `Не удалось скачать файлов: ${count}`,
  installDone: (version: S, runtime: S, downloaded: N, present: N) =>
    `Версия ${version} и Java (${runtime}) готовы: скачано ${downloaded} файлов, ${present} уже были на месте.`,
  progress: (done: N, total: N, doneBytes: S, totalBytes: S) => `${done} / ${total} файлов · ${doneBytes} / ${totalBytes}`,
  progressPresent: (count: N) => ` · ${count} уже на месте`,
  runtimeMissing: (component: S, platform: S) => `Mojang не публикует Java-рантайм ${component} для ${platform}`,
  runtimeNoModules: (component: S) => `В Java-рантайме ${component} нет lib/modules`,

  // --- settings ---
  settingsTitle: 'Настройки',
  systemCheckTitle: 'Проверка браузера',
  systemCheckOk: 'В этом браузере есть всё, что нужно игре.',
  systemCheckProblems: 'Не хватает возможностей, нужных игре:',

  // --- launch plan ---
  planTitle: 'План запуска',
  planArgv: 'Командная строка JVM',
  planVersion: 'Версия',
  planJava: 'Java',
  planJavaValue: (major: N, component: S) => `${major} (${component} от Mojang)`,
  planMainClass: 'Главный класс',
  planClasspath: 'Classpath',
  planClasspathValue: (jars: N) => `${jars} jar`,
  planNatives: 'Нативные модули (реализованы в нашей JVM)',
  planUnresolved: 'Незаполненные переменные',
  hidden: '<скрыт>',

  // --- game ---
  loadingJvm: 'Загружаю JVM…',
  gameStarted: 'Игра запущена',
  jvmExited: (code: N) => `— JVM завершилась с кодом ${code}`,
  confirmStop: 'Остановить игру? Несохранённый прогресс будет потерян.',
  stop: 'Остановить',

  // --- privacy ---
  privacyTitle: 'Конфиденциальность',
  privacySignInHtml:
    'Вход выполняется на серверах Microsoft, Xbox Live и Minecraft. Запросы к ним проходят через наш прокси (<code>/proxy/…</code>) ' +
    'только потому, что эти серверы не разрешают запросы со сторонних сайтов (CORS). Прокси передаёт запросы как есть и не сохраняет ' +
    'и не записывает в журналы ни содержимое запросов и ответов, ни заголовки Authorization, ни токены.',
  privacyTokensHtml:
    'Токены (Microsoft и Minecraft) и ваш профиль хранятся только в этом браузере (localStorage) и удаляются кнопкой «Выйти». ' +
    'Токен Minecraft передаётся игре, которая работает здесь же, в браузере, — как это делает официальный лаунчер.',
  privacyFilesHtml:
    'Файлы игры скачиваются с серверов Mojang в хранилище этого браузера (OPFS). Библиотеки и ассеты идут через тот же прокси ' +
    '(по той же причине — CORS) потоком, без сохранения у нас.',
  privacySettingsHtml:
    'Выбранная версия, принятие EULA, язык и другие настройки хранятся в localStorage этого браузера. ' +
    'Мы не используем cookies, аналитику и рекламу.',

  // --- storage ---
  storageTitle: 'Хранилище',
  storageNote: 'Всё скачивается с серверов Mojang и хранится только в этом браузере (OPFS). Миры и настройки игры лежат там же.',
  clearDownloads: 'Удалить скачанные файлы',
  clearAll: 'Удалить всё, включая миры',
  storageUsage: (used: S, quota: S) => `Занято: ${used} · квота браузера: ${quota}`,
  storagePersistent: ' · хранилище постоянное',
  storageEvictable: ' · браузер может очистить хранилище при нехватке места',
  storageUnavailable: (error: S) => `Хранилище недоступно: ${error}`,
  confirmClearAll: 'Удалить ВСЁ: скачанные файлы, миры, настройки и скриншоты?',
  confirmClearDownloads: 'Удалить скачанные версии, библиотеки, ассеты и Java? Миры и настройки останутся.',
  clearedAll: 'Все файлы игры удалены.',
  clearedDownloads: 'Скачанные файлы удалены.',
  byteUnits: 'Б КБ МБ ГБ ТБ',

  // --- footer ---
  disclaimerLocal: 'НЕОФИЦИАЛЬНЫЙ ПРОДУКТ MINECRAFT. НЕ ОДОБРЕН MOJANG ИЛИ MICROSOFT И НЕ СВЯЗАН С НИМИ.',
  footerHtml:
    'Minecraft — торговая марка Mojang Synergies AB. Игра и её файлы загружаются с серверов Mojang; для игры нужна собственная лицензия. ' +
    '<a href="third-party-notices.txt" target="_blank" rel="noopener">Лицензии стороннего ПО</a>',

  text: (message: S) => message,
};
