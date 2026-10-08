;;; screencast.el --- Record typst-canvas-demo, for bin/screencast.sh -*- lexical-binding: t -*-

;; Fills the X screen with a `modus-vivendi' frame, runs `typst-canvas-demo', and records the
;; screen with ffmpeg (x11grab): from the first shown pages to the end of the demo.  Environment:
;; - TYPST_CANVAS_SCREENCAST: the scenario.  "demo" (default) runs the whole demo.  "inline-math"
;;   hides the preview window, and types an equation, to show the equation below its line.
;; - TYPST_CANVAS_SCREENCAST_OUTPUT: the lossless recording, from which bin/screencast.sh encodes
;;   the MP4 and the GIF.
;; Exits with 1 if the demo or ffmpeg fails.

(require 'typst-canvas-demo)

(defconst typst-canvas-screencast--root
  (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))

(defconst typst-canvas-screencast--timeout 180
  "Seconds after which the run fails.")

(defconst typst-canvas-screencast--frame-rate 24)

(defconst typst-canvas-screencast--scenario (or (getenv "TYPST_CANVAS_SCREENCAST") "demo"))

(defconst typst-canvas-screencast--output
  (or (getenv "TYPST_CANVAS_SCREENCAST_OUTPUT")
      (expand-file-name "target/screencast-raw.mkv" typst-canvas-screencast--root)))

(defconst typst-canvas-screencast--font-height
  (if (equal typst-canvas-screencast--scenario "inline-math") 160 130)
  "Height of the `default' face, in 1/10 pt.  Text must stay legible in a scaled-down GIF.")

(defconst typst-canvas-screencast--inline-math-script
  '((wait-for-preview)
    (pause 1.2)
    (goto "dif x $\n")
    (type "\nEuler's identity, and a sum of binomials:\n")
    (snippet "$ " " $\n")
    (type "e^(i pi) + 1 = 0, quad sum_(k=0)^n binom(n, k) = 2^n")
    (pause 1.5)
    ;; Into the Fourier transform, then out of math.
    (goto "hat(f)(xi)")
    (pause 1.8)
    (goto "Maxwell's equations")
    (pause 1.5))
  "Steps of the \"inline-math\" scenario.  See `typst-canvas-demo--run'.")

(defconst typst-canvas-screencast--tail 1.5
  "Seconds to record after the demo ends, so that its last step shows.")

(defconst typst-canvas-screencast--poll-delay 0.05)

(defvar typst-canvas-screencast--ffmpeg nil "The recording ffmpeg process.")

(defvar typst-canvas-screencast--result nil "How the demo ended, e.g. \"finished\".")

(defun typst-canvas-screencast--fill-screen ()
  "Make the selected frame cover the whole screen.
Xvfb has no window manager, which could make it fullscreen.  A resize
takes effect later, and changes the decoration sizes that this uses, so
call it again after the first one."
  (setq frame-resize-pixelwise t)
  (set-frame-position nil 0 0)
  (set-frame-size nil
                  (- (display-pixel-width) (- (frame-outer-width) (frame-text-width)))
                  (- (display-pixel-height) (- (frame-outer-height) (frame-text-height)))
                  t))

(defun typst-canvas-screencast--when (predicate function)
  "Call FUNCTION as soon as PREDICATE returns non-nil.  Check from a timer."
  (if (funcall predicate)
      (funcall function)
    (run-with-timer typst-canvas-screencast--poll-delay nil
                    #'typst-canvas-screencast--when predicate function)))

(defun typst-canvas-screencast--start-recording ()
  "Start ffmpeg, which records the screen."
  (setq typst-canvas-screencast--ffmpeg
        (make-process
         :name "ffmpeg"
         :buffer "*ffmpeg*"
         :connection-type 'pipe
         :noquery t
         :sentinel #'ignore
         :command (list "ffmpeg" "-loglevel" "error" "-y"
                        "-f" "x11grab" "-draw_mouse" "0"
                        "-framerate" (number-to-string typst-canvas-screencast--frame-rate)
                        "-video_size" (format "%dx%d" (display-pixel-width) (display-pixel-height))
                        "-i" (getenv "DISPLAY")
                        ;; Lossless and cheap to encode while Emacs works.  The outputs are encoded
                        ;; from it later.
                        "-c:v" "libx264" "-preset" "ultrafast" "-qp" "0"
                        typst-canvas-screencast--output))))

(defun typst-canvas-screencast--stop-recording ()
  "Stop ffmpeg, and exit with its status, or 1 if the demo did not finish."
  (let ((ffmpeg typst-canvas-screencast--ffmpeg))
    ;; "q" on stdin makes ffmpeg finish the file and exit.
    (process-send-string ffmpeg "q")
    (while (process-live-p ffmpeg)
      (accept-process-output ffmpeg 0.1))
    (with-current-buffer (process-buffer ffmpeg)
      (unless (zerop (buffer-size))
        (princ (buffer-string) #'external-debugging-output)))
    (unless (equal typst-canvas-screencast--result "finished")
      (princ (format "Demo %s\n" typst-canvas-screencast--result) #'external-debugging-output))
    (kill-emacs (if (and (equal typst-canvas-screencast--result "finished")
                         (zerop (process-exit-status ffmpeg)))
                    0
                  1))))

(defun typst-canvas-screencast--run ()
  "Run the demo, and record it."
  (if (equal typst-canvas-screencast--scenario "inline-math")
      (let ((typst-canvas-display-action '(display-buffer-no-window (allow-no-window . t))))
        (typst-canvas-demo typst-canvas-screencast--inline-math-script))
    (typst-canvas-demo))
  (with-current-buffer typst-canvas-demo--buffer
    (visual-line-mode 1))
  (typst-canvas-screencast--when
   (lambda ()
     (or (null typst-canvas-demo--buffer)
         (with-current-buffer typst-canvas-demo--buffer
           (typst-canvas-demo--preview-ready-p))))
   #'typst-canvas-screencast--start-recording)
  (typst-canvas-screencast--when
   (lambda () (null typst-canvas-demo--buffer))
   (lambda ()
     (run-with-timer typst-canvas-screencast--tail nil
                     #'typst-canvas-screencast--stop-recording))))

(advice-add 'typst-canvas-demo--end :before
            (lambda (how) (setq typst-canvas-screencast--result how)))
(menu-bar-mode -1)
(tool-bar-mode -1)
(scroll-bar-mode -1)
(blink-cursor-mode -1)
(set-face-attribute 'default nil :height typst-canvas-screencast--font-height)
(load-theme 'modus-vivendi t)
(typst-canvas-screencast--fill-screen)
(run-with-timer typst-canvas-screencast--timeout nil #'kill-emacs 1)
(run-with-timer 1 nil #'typst-canvas-screencast--fill-screen)
;; Start after the frame has its final size, so that the preview has its final width.
(run-with-timer 2 nil #'typst-canvas-screencast--run)

;;; screencast.el ends here
