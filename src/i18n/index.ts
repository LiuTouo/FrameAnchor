import { addMessages, init } from 'svelte-i18n';
import zhTW from './zh-TW.json';
import en from './en.json';

addMessages('zh-TW', zhTW);
addMessages('en', en);

init({
  fallbackLocale: 'zh-TW',
  initialLocale: 'zh-TW', // PLAN §3：預設繁體中文
});
