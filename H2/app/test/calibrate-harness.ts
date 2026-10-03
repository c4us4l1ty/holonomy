/**
 * Calibration page. Marks the app as ready so the driver can tell a loaded page
 * from a failed one.
 *
 * The probe element is styled by `calibrate.html`, not here, so the styles
 * cannot drift from the app's.
 */

document.getElementById('probe')!.innerHTML = '<p>probe</p>'

;(window as any).CALIBRATE = {
  ready: true,
  /** The driver replaces the probe's content and reads the rect. */
  probe: () => document.getElementById('probe'),
}

console.log('[holonomy] calibrate ready')
