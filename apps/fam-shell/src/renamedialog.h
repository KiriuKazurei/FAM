#ifndef RENAMEDIALOG_H
#define RENAMEDIALOG_H

#include <QDialog>
#include <QString>

class FamBridge;
class QLabel;
class QLineEdit;

/* Rename one file or folder: the old name, a line to type the new one into, and the transfer
 * primitive's own refusals. Renaming is a move - `fam_path_op` with the folder kept and the name
 * swapped - so "never overwrites" is the rule the OK button runs into, and its sentence is what
 * the note line shows when it says no.
 *
 * The operations are public for the same reason the tag and note dialogs' are: a modal is the one
 * thing a run with no hands cannot drive, so the headless step calls `applyName` directly instead
 * of inventing a second path to the same call. */
class RenameDialog final : public QDialog {
    Q_OBJECT

public:
    RenameDialog(FamBridge *bridge, const QString &path, QWidget *parent = nullptr);

    QString nameText() const;
    void setNameText(const QString &text);
    /* The name as it was when the dialog opened, and the full destination path the last successful
     * apply produced - the two facts an undo entry needs. */
    QString fromPath() const { return m_from; }
    QString toPath() const { return m_to; }
    /* Writes the new name. Refuses a blank locally; everything else is the transfer's sentence. */
    bool applyName(QString *error);
    bool changed() const { return m_changed; }
    QString noteLine() const;

private:
    FamBridge *m_bridge;
    QString m_path;
    QString m_from;
    QString m_to;
    QLineEdit *m_entry = nullptr;
    QLabel *m_line = nullptr;
    bool m_changed = false;
};

#endif /* RENAMEDIALOG_H */
