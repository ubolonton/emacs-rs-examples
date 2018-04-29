(require 'osx)

(ert-deftest contacts ()
  (dolist (contact (osx/find-contacts "Tuấn Anh"))
    (message "%S" contact)))
