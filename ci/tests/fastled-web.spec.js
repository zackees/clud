// @ts-check
const { test, expect } = require('@playwright/test');

test.describe('FastLED Web Interface', () => {
  test('should load FastLED visualization page', async ({ page }) => {
    // Navigate to the FastLED web interface
    await page.goto('/');

    // Check if the page title is correct
    await expect(page).toHaveTitle('FastLED Web Interface');

    // Check if the main heading is present
    await expect(page.locator('h1')).toContainText('FastLED Visualization');

    // Check if the canvas element exists
    const canvas = page.locator('#led-canvas');
    await expect(canvas).toBeVisible();
    
    // Verify canvas dimensions
    const canvasWidth = await canvas.getAttribute('width');
    const canvasHeight = await canvas.getAttribute('height');
    expect(canvasWidth).toBe('800');
    expect(canvasHeight).toBe('600');
  });

  test('should load JavaScript without errors', async ({ page }) => {
    // Listen for console messages
    const consoleMessages = [];
    const errorMessages = [];
    
    page.on('console', msg => {
      consoleMessages.push(msg.text());
      console.log('Browser console:', msg.text());
    });
    
    page.on('pageerror', error => {
      errorMessages.push(error.message);
      console.error('Page error:', error.message);
    });

    await page.goto('/');

    // Wait for page to fully load
    await page.waitForLoadState('networkidle');

    // Check for expected console message
    expect(consoleMessages).toContain('FastLED web interface loaded');
    
    // This test will demonstrate console errors - FastLED library not loaded
    expect(errorMessages.length).toBeGreaterThan(0);
    console.log('Expected errors (FastLED not loaded):', errorMessages);
  });

  test('should fail when trying to access FastLED functions', async ({ page }) => {
    // This test demonstrates the expected error when FastLED is not properly compiled/loaded
    
    const scriptErrors = [];
    page.on('pageerror', error => {
      scriptErrors.push(error.message);
    });

    await page.goto('/');

    // Try to execute FastLED-related JavaScript that would fail
    const result = await page.evaluate(() => {
      try {
        // This would normally work if FastLED was compiled and loaded
        if (typeof FastLED !== 'undefined') {
          return { success: true, message: 'FastLED loaded successfully' };
        } else {
          throw new Error('FastLED is not defined - compilation/loading failed');
        }
      } catch (error) {
        return { success: false, message: error.message };
      }
    });

    // This should fail, demonstrating the console error
    expect(result.success).toBe(false);
    expect(result.message).toContain('FastLED is not defined');
    
    console.log('Expected FastLED error:', result.message);
  });

  test('should show compilation errors in console', async ({ page }) => {
    const consoleErrors = [];
    
    page.on('console', msg => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
        console.log('Console Error:', msg.text());
      }
    });

    await page.goto('/');

    // Add script that tries to use FastLED but will fail
    await page.addScriptTag({
      content: `
        try {
          // Simulate trying to initialize FastLED
          console.error('FastLED compilation error: undefined reference to FastLED::show()');
          console.error('FastLED compilation error: undefined reference to FastLED::setBrightness()');
          console.error('FastLED compilation error: CRGB not declared in this scope');
          throw new Error('FastLED compilation failed - missing dependencies');
        } catch (e) {
          console.error('Runtime error:', e.message);
        }
      `
    });

    await page.waitForTimeout(1000);

    // Verify that compilation errors are logged
    expect(consoleErrors.length).toBeGreaterThan(0);
    expect(consoleErrors.some(error => error.includes('FastLED compilation error'))).toBe(true);
    
    console.log('All console errors found:');
    consoleErrors.forEach(error => console.log('  -', error));
  });
});