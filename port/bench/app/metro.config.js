const {getDefaultConfig} = require('@react-native/metro-config');
const config = getDefaultConfig(__dirname);
config.resolver.useWatchman = false;
module.exports = config;
