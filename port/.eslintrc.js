/**
 * Port tooling is Node scripts that report to the terminal, and black-box
 * tests that use Metro's own dev dependencies (e.g. ws).
 */
'use strict';

module.exports = {
  rules: {
    'no-console': 'off',
    'import/no-extraneous-dependencies': 'off',
  },
};
